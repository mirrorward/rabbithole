//! UPnP-IGD codec — pure builders/parsers for the text protocols a UPnP
//! Internet Gateway Device speaks.
//!
//! Unlike NAT-PMP/PCP, UPnP-IGD is a *text* stack: devices are found via SSDP
//! (a UDP multicast HTTP-ish discovery), and port mappings are created by
//! POSTing SOAP/XML action bodies to a control URL. This module is the pure
//! codec layer — it builds the request text and tolerantly parses responses —
//! with **no live IO**. Two IO steps are deliberately *not* here and are
//! documented as a mapper-layer follow-up:
//!
//! 1. Sending the [`m_search`] datagram and reading the `LOCATION` header of
//!    replies.
//! 2. Fetching the device-descriptor XML from that `LOCATION` to learn the
//!    service **control URL** (and the exact `WANIPConnection` /
//!    `WANPPPConnection` service type) to POST the SOAP bodies at.
//!
//! Everything text-shaped is testable with pinned request bytes and sample
//! response parsing.

use crate::Protocol;

/// The IPv4 SSDP multicast group and port an `M-SEARCH` is sent to.
pub const SSDP_MULTICAST: &str = "239.255.255.250:1900";

/// The IGD v1 root-device search target.
pub const ST_IGD: &str = "urn:schemas-upnp-org:device:InternetGatewayDevice:1";

/// The WANIPConnection v1 service type (the usual control service).
pub const SERVICE_WANIP: &str = "urn:schemas-upnp-org:service:WANIPConnection:1";

/// Build an SSDP `M-SEARCH` discovery datagram for the given search target
/// (`st`), e.g. [`ST_IGD`]. `mx` is the max wait (seconds) a device should
/// spread its reply over. Lines are CRLF-terminated and the datagram ends with
/// a blank line, per the SSDP/HTTPU convention.
pub fn m_search(st: &str, mx: u32) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: {SSDP_MULTICAST}\r\n\
         MAN: \"ssdp:discover\"\r\n\
         MX: {mx}\r\n\
         ST: {st}\r\n\
         \r\n"
    )
}

/// Extract the `LOCATION` header (the device-descriptor URL) from an SSDP
/// search reply. Case-insensitive on the header name; returns the trimmed
/// value, or `None` if absent. Tolerant of CRLF or bare-LF line endings.
pub fn parse_location(reply: &str) -> Option<String> {
    header_value(reply, "location")
}

/// The SOAPAction HTTP header value for `action` on [`SERVICE_WANIP`], e.g.
/// `"urn:schemas-upnp-org:service:WANIPConnection:1#AddPortMapping"` (quotes
/// included, as the header requires).
pub fn soap_action_header(service_type: &str, action: &str) -> String {
    format!("\"{service_type}#{action}\"")
}

/// Wrap an action body in the SOAP envelope every IGD control request uses.
fn envelope(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\r\n\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body>{body}</s:Body></s:Envelope>"
    )
}

/// Build the SOAP body for `AddPortMapping` on `service_type`.
///
/// `external_port` is the port to open on the WAN side; `internal_port` and
/// `internal_client` (the LAN IP as a dotted string) are where traffic is
/// forwarded; `lease_secs` is the mapping lifetime (0 = the router's default
/// "indefinite", which many routers require for static mappings).
#[allow(clippy::too_many_arguments)]
pub fn add_port_mapping(
    service_type: &str,
    protocol: Protocol,
    external_port: u16,
    internal_port: u16,
    internal_client: &str,
    description: &str,
    lease_secs: u32,
) -> String {
    let body = format!(
        "<u:AddPortMapping xmlns:u=\"{service_type}\">\
         <NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{external_port}</NewExternalPort>\
         <NewProtocol>{proto}</NewProtocol>\
         <NewInternalPort>{internal_port}</NewInternalPort>\
         <NewInternalClient>{internal_client}</NewInternalClient>\
         <NewEnabled>1</NewEnabled>\
         <NewPortMappingDescription>{desc}</NewPortMappingDescription>\
         <NewLeaseDuration>{lease_secs}</NewLeaseDuration>\
         </u:AddPortMapping>",
        proto = protocol.upnp_str(),
        desc = xml_escape(description),
    );
    envelope(&body)
}

/// Build the SOAP body for `DeletePortMapping` on `service_type`.
pub fn delete_port_mapping(service_type: &str, protocol: Protocol, external_port: u16) -> String {
    let body = format!(
        "<u:DeletePortMapping xmlns:u=\"{service_type}\">\
         <NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{external_port}</NewExternalPort>\
         <NewProtocol>{proto}</NewProtocol>\
         </u:DeletePortMapping>",
        proto = protocol.upnp_str(),
    );
    envelope(&body)
}

/// Build the SOAP body for `GetExternalIPAddress` on `service_type`.
pub fn get_external_ip_address(service_type: &str) -> String {
    let body =
        format!("<u:GetExternalIPAddress xmlns:u=\"{service_type}\"></u:GetExternalIPAddress>");
    envelope(&body)
}

/// Extract the external IP from a `GetExternalIPAddressResponse` body: the text
/// of the (namespace-agnostic) `<NewExternalIPAddress>` element. Tolerant of
/// namespace prefixes and surrounding whitespace; returns `None` if absent.
pub fn parse_external_ip(response: &str) -> Option<String> {
    element_text(response, "NewExternalIPAddress").map(|s| s.trim().to_string())
}

/// The outcome of parsing a SOAP action response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoapResult {
    /// The action succeeded (no SOAP `<Fault>` was present).
    Ok,
    /// The router returned a SOAP fault carrying a UPnP error code + text.
    Fault {
        /// The `<errorCode>` value, if it parsed as a number.
        code: Option<u16>,
        /// The `<errorDescription>` text, if present.
        description: Option<String>,
    },
}

/// Tolerantly classify a SOAP action response as success or a typed fault.
/// A body containing `<...Fault>` (any namespace prefix) is a fault; its
/// `<errorCode>`/`<errorDescription>` are extracted best-effort.
pub fn parse_soap_result(response: &str) -> SoapResult {
    if !contains_element(response, "Fault") {
        return SoapResult::Ok;
    }
    let code = element_text(response, "errorCode").and_then(|s| s.trim().parse::<u16>().ok());
    let description = element_text(response, "errorDescription").map(|s| s.trim().to_string());
    SoapResult::Fault { code, description }
}

// ---- tolerant text helpers (no XML dependency) --------------------------

/// Minimal XML text escaping for values we inject into request bodies.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Fetch an HTTP header value (case-insensitive name), handling CRLF/LF.
fn header_value(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(name) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Return the text content of the first element whose *local* name is `local`
/// (ignoring any `ns:` prefix and any attributes). Tolerant, allocation-light,
/// and namespace-agnostic — enough for the small, well-formed IGD responses.
fn element_text(xml: &str, local: &str) -> Option<String> {
    let (open_start, open_end) = find_open_tag(xml, local)?;
    let after = &xml[open_end..];
    // The matching close tag, again namespace-agnostic: search for "</...local>".
    let close = find_close_tag(after, local)?;
    let _ = open_start;
    Some(after[..close].to_string())
}

/// Whether an element with local name `local` appears at all (open tag).
fn contains_element(xml: &str, local: &str) -> bool {
    find_open_tag(xml, local).is_some()
}

/// Find the first opening tag `<[ns:]local ...>`; return (tag start, index just
/// past the `>`).
fn find_open_tag(xml: &str, local: &str) -> Option<(usize, usize)> {
    let bytes = xml.as_bytes();
    let mut i = 0;
    while let Some(rel) = xml[i..].find('<') {
        let lt = i + rel;
        // Skip closing tags and declarations/comments.
        let next = bytes.get(lt + 1).copied();
        if next == Some(b'/') || next == Some(b'?') || next == Some(b'!') {
            i = lt + 1;
            continue;
        }
        // Read the tag name up to whitespace or '>' or '/'.
        let name_start = lt + 1;
        let mut j = name_start;
        while j < bytes.len() {
            let c = bytes[j];
            if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' || c == b'>' || c == b'/' {
                break;
            }
            j += 1;
        }
        let name = &xml[name_start..j];
        let local_name = name.rsplit(':').next().unwrap_or(name);
        if local_name == local {
            // Advance to just past the '>'.
            if let Some(gt_rel) = xml[j..].find('>') {
                return Some((lt, j + gt_rel + 1));
            }
            return None;
        }
        i = lt + 1;
    }
    None
}

/// Find the first closing tag `</[ns:]local>`; return the byte index of its
/// `<` within `xml`.
fn find_close_tag(xml: &str, local: &str) -> Option<usize> {
    let bytes = xml.as_bytes();
    let mut i = 0;
    while let Some(rel) = xml[i..].find("</") {
        let lt = i + rel;
        let name_start = lt + 2;
        let mut j = name_start;
        while j < bytes.len() {
            let c = bytes[j];
            if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' || c == b'>' {
                break;
            }
            j += 1;
        }
        let name = &xml[name_start..j];
        let local_name = name.rsplit(':').next().unwrap_or(name);
        if local_name == local {
            return Some(lt);
        }
        i = lt + 2;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m_search_has_required_headers() {
        let ds = m_search(ST_IGD, 2);
        assert!(ds.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(ds.contains("HOST: 239.255.255.250:1900\r\n"));
        assert!(ds.contains("MAN: \"ssdp:discover\"\r\n"));
        assert!(ds.contains("MX: 2\r\n"));
        assert!(ds.ends_with("\r\n\r\n"));
    }

    #[test]
    fn location_parses_case_insensitively() {
        let reply = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\n\
                     Location: http://192.168.1.1:5000/rootDesc.xml\r\n\r\n";
        assert_eq!(
            parse_location(reply).as_deref(),
            Some("http://192.168.1.1:5000/rootDesc.xml")
        );
    }

    #[test]
    fn external_ip_extracts_from_soap() {
        let resp = "<?xml version=\"1.0\"?><s:Envelope><s:Body>\
             <u:GetExternalIPAddressResponse xmlns:u=\"urn:...\">\
             <NewExternalIPAddress>203.0.113.7</NewExternalIPAddress>\
             </u:GetExternalIPAddressResponse></s:Body></s:Envelope>";
        assert_eq!(parse_external_ip(resp).as_deref(), Some("203.0.113.7"));
    }

    #[test]
    fn soap_fault_is_typed() {
        let ok = "<s:Envelope><s:Body><u:AddPortMappingResponse/></s:Body></s:Envelope>";
        assert_eq!(parse_soap_result(ok), SoapResult::Ok);

        let fault = "<s:Envelope><s:Body><s:Fault><faultcode>s:Client</faultcode>\
             <detail><UPnPError><errorCode>718</errorCode>\
             <errorDescription>ConflictInMappingEntry</errorDescription>\
             </UPnPError></detail></s:Fault></s:Body></s:Envelope>";
        assert_eq!(
            parse_soap_result(fault),
            SoapResult::Fault {
                code: Some(718),
                description: Some("ConflictInMappingEntry".into()),
            }
        );
    }
}
