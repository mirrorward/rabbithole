//! Is a bookmarked burrow up?
//!
//! A directory reports health for the burrows it lists. A bookmark exists
//! precisely for the ones it may not: a burrow that dropped off the list, one
//! on your own network, a friend's that never announced. For those the only
//! honest source is to knock: open the same WebSocket a connection would, and
//! see whether anyone answers. Nothing is sent, and the socket closes at once.
//!
//! A probe reaches the address the person bookmarked, which is what pressing
//! Connect would do. It goes through the same endpoint rule as a connection
//! ([`rabbithole_core::api::normalize_secure_ws_endpoint`]), so a probe never
//! dials something the client would refuse to connect to.

/// What a knock found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// Asked, no answer yet.
    Checking,
    /// The burrow answered.
    Up,
    /// No answer, a refusal, or an address the client won't dial.
    Down,
}

impl Probe {
    /// As a row's reachability: unknown while the knock is still out.
    pub fn reachable(self) -> Option<bool> {
        match self {
            Probe::Checking => None,
            Probe::Up => Some(true),
            Probe::Down => Some(false),
        }
    }
}

/// How long a burrow gets to answer.
pub const PROBE_TIMEOUT_MS: u32 = 5_000;

/// Knock on `endpoint` and report once: `true` if a WebSocket opened.
#[cfg(target_arch = "wasm32")]
pub fn knock(endpoint: &str, report: impl FnOnce(bool) + 'static) {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    let Ok(url) = rabbithole_core::api::normalize_secure_ws_endpoint(endpoint) else {
        report(false);
        return;
    };
    let Ok(ws) = web_sys::WebSocket::new(&url) else {
        report(false);
        return;
    };
    // One answer, whichever of open / error / close / timeout comes first.
    let report: Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>> =
        Rc::new(RefCell::new(Some(Box::new(report))));
    let settle = {
        let ws = ws.clone();
        let report = report.clone();
        move |up: bool| {
            if let Some(report) = report.borrow_mut().take() {
                ws.set_onopen(None);
                ws.set_onerror(None);
                ws.set_onclose(None);
                let _ = ws.close();
                report(up);
            }
        }
    };
    let on_open = {
        let settle = settle.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| settle(true))
    };
    let on_fail = {
        let settle = settle.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| settle(false))
    };
    ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    ws.set_onerror(Some(on_fail.as_ref().unchecked_ref()));
    ws.set_onclose(Some(on_fail.as_ref().unchecked_ref()));
    // The closures live until the knock settles, and are dropped a tick
    // later: never from inside their own call.
    wasm_bindgen_futures::spawn_local(async move {
        let mut waited = 0;
        while report.borrow().is_some() && waited < PROBE_TIMEOUT_MS {
            gloo_timers::future::TimeoutFuture::new(100).await;
            waited += 100;
        }
        settle(false);
        gloo_timers::future::TimeoutFuture::new(0).await;
        drop(on_open);
        drop(on_fail);
    });
}

#[cfg(test)]
mod tests {
    use super::Probe;

    #[test]
    fn a_knock_still_out_is_unknown_not_down() {
        assert_eq!(Probe::Checking.reachable(), None);
        assert_eq!(Probe::Up.reachable(), Some(true));
        assert_eq!(Probe::Down.reachable(), Some(false));
    }
}
