//! Transient **toast notifications** — the "you've got mail" humanized-event
//! moments (PLAN §45). DOM-free and host-tested: a small queue with a cap and
//! stable ids; the view ([`crate::components`]) renders it into an
//! `aria-live` region and drives auto-dismiss.

/// The flavour of a toast, which the view maps to an icon + accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Neutral information.
    Info,
    /// A positive outcome (connected, saved).
    Success,
    /// New mail / a direct message arrived.
    Mail,
    /// Something needs attention (a soft warning, never an error dialog).
    Warn,
}

impl ToastKind {
    /// A stable glyph for the toast (decorative; the text carries meaning).
    pub fn glyph(self) -> &'static str {
        match self {
            ToastKind::Info => "\u{2139}",    // ℹ
            ToastKind::Success => "\u{2713}", // ✓
            ToastKind::Mail => "\u{2709}",    // ✉
            ToastKind::Warn => "\u{26A0}",    // ⚠
        }
    }

    /// A CSS modifier class suffix for styling.
    pub fn class(self) -> &'static str {
        match self {
            ToastKind::Info => "info",
            ToastKind::Success => "success",
            ToastKind::Mail => "mail",
            ToastKind::Warn => "warn",
        }
    }
}

/// One live toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// Stable id for keyed rendering + targeted dismissal.
    pub id: u64,
    pub kind: ToastKind,
    pub text: String,
}

/// The toast queue: newest last, capped so a burst can't grow unbounded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToastQueue {
    items: Vec<Toast>,
    next_id: u64,
}

/// The most toasts kept on screen at once; older ones drop off the front.
pub const MAX_TOASTS: usize = 4;

impl ToastQueue {
    /// Push a toast, returning its id. Trims the oldest beyond [`MAX_TOASTS`].
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Toast {
            id,
            kind,
            text: text.into(),
        });
        if self.items.len() > MAX_TOASTS {
            let overflow = self.items.len() - MAX_TOASTS;
            self.items.drain(0..overflow);
        }
        id
    }

    /// Remove a toast by id (a no-op if already gone).
    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|t| t.id != id);
    }

    /// The live toasts, oldest first.
    pub fn items(&self) -> &[Toast] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_returns_unique_ids_and_appends() {
        let mut q = ToastQueue::default();
        let a = q.push(ToastKind::Info, "one");
        let b = q.push(ToastKind::Mail, "two");
        assert_ne!(a, b);
        assert_eq!(q.items().len(), 2);
        assert_eq!(q.items()[0].text, "one");
        assert_eq!(q.items()[1].kind, ToastKind::Mail);
    }

    #[test]
    fn dismiss_removes_only_the_target() {
        let mut q = ToastQueue::default();
        let a = q.push(ToastKind::Info, "one");
        let b = q.push(ToastKind::Info, "two");
        q.dismiss(a);
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].id, b);
        // Dismissing a gone id is a no-op.
        q.dismiss(a);
        assert_eq!(q.items().len(), 1);
    }

    #[test]
    fn queue_is_capped_dropping_oldest() {
        let mut q = ToastQueue::default();
        let mut ids = Vec::new();
        for i in 0..(MAX_TOASTS + 2) {
            ids.push(q.push(ToastKind::Info, format!("n{i}")));
        }
        assert_eq!(q.items().len(), MAX_TOASTS);
        // The two oldest were trimmed; ids stay monotonic (no reuse).
        assert_eq!(q.items()[0].text, "n2");
        assert!(q.items().iter().all(|t| t.id >= ids[2]));
    }

    #[test]
    fn kinds_have_distinct_glyphs_and_classes() {
        let kinds = [
            ToastKind::Info,
            ToastKind::Success,
            ToastKind::Mail,
            ToastKind::Warn,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(a.glyph(), b.glyph());
                assert_ne!(a.class(), b.class());
            }
        }
    }
}
