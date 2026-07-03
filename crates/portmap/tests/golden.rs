//! Golden byte-vector tests for the NAT-PMP, PCP, and UPnP codecs.
//!
//! Each fixture pins the exact wire bytes of a representative message per the
//! relevant RFC's field layout, then asserts `decode → typed value` and
//! `typed value → encode` are a fixed point in both directions. Totality /
//! never-panic sweeps live in `tests/fuzz.rs`.

#![forbid(unsafe_code)]

use std::net::Ipv4Addr;

use rabbithole_portmap::{natpmp, pcp, upnp, Protocol};

// ---- NAT-PMP (RFC 6886) --------------------------------------------------

/// External-address request (2 bytes): `vers=0`, `op=0`.
const NATPMP_EXT_REQ: &[u8] = b"\x00\x00";

/// External-address response (12 bytes):
/// ```text
/// 00        vers = 0
/// 80        op   = 128 (0 + response bit)
/// 00 00     result = 0 (success)
/// 00 01 E2 40   seconds-of-epoch = 123456
/// CB 00 71 05   external IPv4 = 203.0.113.5
/// ```
const NATPMP_EXT_RESP: &[u8] = b"\x00\x80\x00\x00\x00\x01\xE2\x40\xCB\x00\x71\x05";

/// MAP request, UDP (12 bytes):
/// ```text
/// 00        vers = 0
/// 01        op   = 1 (map UDP)
/// 00 00     reserved
/// 12 2D     internal port = 4653
/// 12 2D     suggested external port = 4653
/// 00 00 1C 20   lifetime = 7200
/// ```
const NATPMP_MAP_REQ: &[u8] = b"\x00\x01\x00\x00\x12\x2D\x12\x2D\x00\x00\x1C\x20";

/// MAP response, UDP (16 bytes):
/// ```text
/// 00        vers = 0
/// 81        op   = 129 (1 + response bit)
/// 00 00     result = 0 (success)
/// 00 01 E2 40   seconds-of-epoch = 123456
/// 12 2D     internal port = 4653
/// 87 07     mapped external port = 34567
/// 00 00 0E 10   lifetime = 3600
/// ```
const NATPMP_MAP_RESP: &[u8] = b"\x00\x81\x00\x00\x00\x01\xE2\x40\x12\x2D\x87\x07\x00\x00\x0E\x10";

#[test]
fn natpmp_external_request_is_golden() {
    assert_eq!(natpmp::ExternalAddressRequest.encode(), NATPMP_EXT_REQ);
}

#[test]
fn natpmp_external_response_is_golden() {
    let resp = natpmp::ExternalAddressResponse::decode(NATPMP_EXT_RESP).unwrap();
    assert_eq!(resp.result, natpmp::ResultCode::Success);
    assert_eq!(resp.epoch, 123456);
    assert_eq!(resp.external_ip, Ipv4Addr::new(203, 0, 113, 5));
    assert_eq!(resp.encode(), NATPMP_EXT_RESP);
}

#[test]
fn natpmp_map_request_is_golden() {
    let req = natpmp::MapRequest::decode(NATPMP_MAP_REQ).unwrap();
    assert_eq!(req.protocol, Protocol::Udp);
    assert_eq!(req.internal_port, 4653);
    assert_eq!(req.suggested_external_port, 4653);
    assert_eq!(req.lifetime_secs, 7200);
    assert_eq!(req.encode(), NATPMP_MAP_REQ);
}

#[test]
fn natpmp_map_response_is_golden() {
    let resp = natpmp::MapResponse::decode(NATPMP_MAP_RESP).unwrap();
    assert_eq!(resp.protocol, Protocol::Udp);
    assert_eq!(resp.result, natpmp::ResultCode::Success);
    assert_eq!(resp.epoch, 123456);
    assert_eq!(resp.internal_port, 4653);
    assert_eq!(resp.external_port, 34567);
    assert_eq!(resp.lifetime_secs, 3600);
    assert_eq!(resp.encode(), NATPMP_MAP_RESP);
}

// ---- PCP (RFC 6887) ------------------------------------------------------

/// MAP request (60 bytes): version byte 2 distinguishes PCP from NAT-PMP (0).
/// ```text
/// 02 01 00 00                             vers=2, op=MAP (R=0), reserved
/// 00 00 1C 20                             requested lifetime = 7200
/// 00*10 FF FF C0 A8 01 32                 client IP ::ffff:192.168.1.50
/// AA*12                                   mapping nonce
/// 11                                      protocol = 17 (UDP)
/// 00 00 00                                reserved
/// 12 2D                                   internal port = 4653
/// 12 2D                                   suggested external port = 4653
/// 00*16                                   suggested external IP = :: (any)
/// ```
const PCP_MAP_REQ: &[u8] = b"\x02\x01\x00\x00\x00\x00\x1C\x20\
\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xFF\xFF\xC0\xA8\x01\x32\
\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\
\x11\x00\x00\x00\x12\x2D\x12\x2D\
\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";

/// MAP response (60 bytes):
/// ```text
/// 02 81 00 00                             vers=2, op=MAP|R, reserved, result=0
/// 00 00 0E 10                             granted lifetime = 3600
/// 00 00 00 2A                             epoch = 42
/// 00*12                                   reserved (96 bits)
/// BB*12                                   mapping nonce (echoed)
/// 06                                      protocol = 6 (TCP)
/// 00 00 00                                reserved
/// 12 2E                                   internal port = 4654
/// C7 38                                   assigned external port = 51000
/// 00*10 FF FF CB 00 71 09                 assigned IP ::ffff:203.0.113.9
/// ```
const PCP_MAP_RESP: &[u8] = b"\x02\x81\x00\x00\x00\x00\x0E\x10\x00\x00\x00\x2A\
\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
\xBB\xBB\xBB\xBB\xBB\xBB\xBB\xBB\xBB\xBB\xBB\xBB\
\x06\x00\x00\x00\x12\x2E\xC7\x38\
\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xFF\xFF\xCB\x00\x71\x09";

#[test]
fn pcp_map_request_is_golden() {
    assert_eq!(PCP_MAP_REQ.len(), pcp::MAP_MSG_LEN);
    let req = pcp::MapRequest::decode(PCP_MAP_REQ).unwrap();
    assert_eq!(req.lifetime_secs, 7200);
    assert_eq!(
        pcp::ip16_to_ipaddr(req.client_ip),
        std::net::IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50))
    );
    assert_eq!(req.nonce, [0xAA; pcp::NONCE_LEN]);
    assert_eq!(req.protocol, 17);
    assert_eq!(req.internal_port, 4653);
    assert_eq!(req.suggested_external_port, 4653);
    assert_eq!(req.encode(), PCP_MAP_REQ);
}

#[test]
fn pcp_map_response_is_golden() {
    assert_eq!(PCP_MAP_RESP.len(), pcp::MAP_MSG_LEN);
    let resp = pcp::MapResponse::decode(PCP_MAP_RESP).unwrap();
    assert_eq!(resp.result, pcp::ResultCode::Success);
    assert_eq!(resp.lifetime_secs, 3600);
    assert_eq!(resp.epoch, 42);
    assert_eq!(resp.nonce, [0xBB; pcp::NONCE_LEN]);
    assert_eq!(resp.protocol, 6);
    assert_eq!(resp.internal_port, 4654);
    assert_eq!(resp.assigned_external_port, 51000);
    assert_eq!(
        pcp::ip16_to_ipaddr(resp.assigned_external_ip),
        std::net::IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))
    );
    assert_eq!(resp.encode(), PCP_MAP_RESP);
}

/// The leading version byte is the sole discriminator between the two binary
/// protocols on the shared UDP port 5351.
#[test]
fn version_byte_distinguishes_natpmp_and_pcp() {
    assert_eq!(NATPMP_MAP_REQ[0], natpmp::VERSION); // 0
    assert_eq!(PCP_MAP_REQ[0], pcp::VERSION); // 2
                                              // Cross-feeding rejects on version, never panics.
    assert!(matches!(
        pcp::MapRequest::decode(NATPMP_MAP_REQ),
        Err(pcp::PcpError::Short { .. }) | Err(pcp::PcpError::BadVersion(0))
    ));
    assert_eq!(
        natpmp::MapResponse::decode(PCP_MAP_RESP),
        Err(natpmp::NatPmpError::BadVersion(2))
    );
}

// ---- UPnP-IGD (SOAP + SSDP text) ----------------------------------------

/// The exact `M-SEARCH` datagram bytes for IGD discovery (CRLF-terminated,
/// trailing blank line).
const SSDP_MSEARCH_GOLDEN: &[u8] = b"M-SEARCH * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
MAN: \"ssdp:discover\"\r\n\
MX: 2\r\n\
ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
\r\n";

#[test]
fn ssdp_m_search_is_golden() {
    assert_eq!(
        upnp::m_search(upnp::ST_IGD, 2).as_bytes(),
        SSDP_MSEARCH_GOLDEN
    );
}

#[test]
fn soap_add_port_mapping_request_is_golden() {
    let body = upnp::add_port_mapping(
        upnp::SERVICE_WANIP,
        Protocol::Udp,
        4653,
        4653,
        "192.168.1.50",
        "RabbitHole QUIC",
        7200,
    );
    // Pin the load-bearing SOAP fields exactly.
    assert!(body.starts_with("<?xml version=\"1.0\"?>"));
    assert!(body
        .contains("<u:AddPortMapping xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">"));
    assert!(body.contains("<NewExternalPort>4653</NewExternalPort>"));
    assert!(body.contains("<NewProtocol>UDP</NewProtocol>"));
    assert!(body.contains("<NewInternalPort>4653</NewInternalPort>"));
    assert!(body.contains("<NewInternalClient>192.168.1.50</NewInternalClient>"));
    assert!(body.contains("<NewEnabled>1</NewEnabled>"));
    assert!(body.contains("<NewPortMappingDescription>RabbitHole QUIC</NewPortMappingDescription>"));
    assert!(body.contains("<NewLeaseDuration>7200</NewLeaseDuration>"));
    assert!(body.ends_with("</s:Envelope>"));

    assert_eq!(
        upnp::soap_action_header(upnp::SERVICE_WANIP, "AddPortMapping"),
        "\"urn:schemas-upnp-org:service:WANIPConnection:1#AddPortMapping\""
    );
}

#[test]
fn soap_delete_and_get_external_ip_requests() {
    let del = upnp::delete_port_mapping(upnp::SERVICE_WANIP, Protocol::Tcp, 4654);
    assert!(del.contains("<u:DeletePortMapping"));
    assert!(del.contains("<NewExternalPort>4654</NewExternalPort>"));
    assert!(del.contains("<NewProtocol>TCP</NewProtocol>"));

    let get = upnp::get_external_ip_address(upnp::SERVICE_WANIP);
    assert!(get.contains("<u:GetExternalIPAddress xmlns:u="));
}

/// Parse a real-shaped `GetExternalIPAddressResponse` and an error fault.
#[test]
fn soap_responses_parse() {
    let ip_resp = "<?xml version=\"1.0\"?>\r\n<s:Envelope><s:Body>\
        <u:GetExternalIPAddressResponse xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">\
        <NewExternalIPAddress>203.0.113.9</NewExternalIPAddress>\
        </u:GetExternalIPAddressResponse></s:Body></s:Envelope>";
    assert_eq!(
        upnp::parse_external_ip(ip_resp).as_deref(),
        Some("203.0.113.9")
    );
    assert_eq!(upnp::parse_soap_result(ip_resp), upnp::SoapResult::Ok);

    let fault = "<s:Envelope><s:Body><s:Fault><faultcode>s:Client</faultcode>\
        <faultstring>UPnPError</faultstring><detail><UPnPError \
        xmlns=\"urn:schemas-upnp-org:control-1-0\">\
        <errorCode>725</errorCode><errorDescription>OnlyPermanentLeasesSupported\
        </errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>";
    assert_eq!(
        upnp::parse_soap_result(fault),
        upnp::SoapResult::Fault {
            code: Some(725),
            description: Some("OnlyPermanentLeasesSupported".into()),
        }
    );
}
