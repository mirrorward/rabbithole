//! Property tests: the PermissionEvaluator vs an independently written
//! reference implementation, over randomized ACL trees and subjects.

use proptest::prelude::*;
use rabbithole_server_core::permissions::{AclRule, PermissionEvaluator, Principal, Role, Subject};

/// Reference implementation, written from the spec (PLAN §7) without
/// looking at the production code path: compute base, collect matching
/// rules per level, apply the nearest level's combined rule.
fn reference_effective(
    rules: &[(String, Principal, AclRule)],
    subject: &Subject,
    resource: &str,
) -> u64 {
    if subject.role == Role::Superuser {
        return u64::MAX;
    }
    let mut caps = subject.role.default_caps().0 | subject.class_mask;
    caps |= subject.grant_mask;
    caps &= !subject.revoke_mask;

    let matches_subject = |p: &Principal| match p {
        Principal::Everyone => true,
        Principal::Role(r) => *r == subject.role,
        Principal::Class(c) => Some(*c) == subject.class_id,
        Principal::Account(a) => *a == subject.account_id,
    };

    // Build levels: resource, then chop segments; "" root is last.
    let mut levels: Vec<String> = Vec::new();
    let mut cur = resource.to_string();
    loop {
        levels.push(cur.clone());
        if cur.is_empty() {
            break;
        }
        cur = match cur.rfind('/') {
            Some(i) => cur[..i].to_string(),
            None => String::new(),
        };
    }

    for level in levels {
        let mut allow = 0u64;
        let mut deny = 0u64;
        let mut any = false;
        for (res, principal, rule) in rules {
            if *res == level && matches_subject(principal) {
                any = true;
                allow |= rule.allow;
                deny |= rule.deny;
            }
        }
        if any {
            caps |= allow;
            caps &= !deny;
            break;
        }
    }
    caps
}

fn arb_role() -> impl Strategy<Value = Role> {
    prop_oneof![
        Just(Role::Guest),
        Just(Role::User),
        Just(Role::Moderator),
        Just(Role::Admin),
        Just(Role::Superuser),
    ]
}

fn arb_principal() -> impl Strategy<Value = Principal> {
    prop_oneof![
        Just(Principal::Everyone),
        arb_role().prop_map(Principal::Role),
        (0i64..4).prop_map(Principal::Class),
        (0i64..4).prop_map(Principal::Account),
    ]
}

/// Small path universe so rules and queries actually collide.
fn arb_path() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("a".to_string()),
        Just("a/b".to_string()),
        Just("a/b/c".to_string()),
        Just("a/x".to_string()),
        Just("d".to_string()),
        Just("d/e".to_string()),
    ]
}

fn arb_rule() -> impl Strategy<Value = AclRule> {
    (any::<u64>(), any::<u64>()).prop_map(|(allow, deny)| AclRule { allow, deny })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn evaluator_matches_reference(
        rules in prop::collection::vec((arb_path(), arb_principal(), arb_rule()), 0..12),
        role in arb_role(),
        class_id in prop_oneof![Just(None), (0i64..4).prop_map(Some)],
        class_mask in any::<u64>(),
        grant in any::<u64>(),
        revoke in any::<u64>(),
        account_id in 0i64..4,
        query in arb_path(),
    ) {
        let subject = Subject { account_id, role, class_id, class_mask, grant_mask: grant, revoke_mask: revoke };

        let mut eval = PermissionEvaluator::new();
        for (res, principal, rule) in &rules {
            // NB: insert replaces per (resource, principal); mirror that in
            // the reference input by keeping only the LAST rule per key.
            eval.insert_rule(res, *principal, *rule);
        }
        let mut deduped: Vec<(String, Principal, AclRule)> = Vec::new();
        for (res, principal, rule) in &rules {
            deduped.retain(|(r, p, _)| !(r == res && p == principal));
            deduped.push((res.clone(), *principal, *rule));
        }

        let expected = reference_effective(&deduped, &subject, &query);
        let actual = eval.effective(&subject, &query);
        prop_assert_eq!(actual, expected);

        // Cache coherence: asking twice yields the same answer.
        prop_assert_eq!(eval.effective(&subject, &query), expected);
    }

    #[test]
    fn added_deny_never_expands_caps(
        rules in prop::collection::vec((arb_path(), arb_principal(), arb_rule()), 0..8),
        role in arb_role(),
        query in arb_path(),
        deny_bits in any::<u64>(),
    ) {
        prop_assume!(role != Role::Superuser);
        // Only meaningful when the query level had no rules yet: inserting
        // into a level that already has rules *replaces* the Everyone rule
        // and re-combines with other principals' rules there, which can
        // legitimately surface bits the old combined deny was masking.
        prop_assume!(rules.iter().all(|(r, _, _)| r != &query));
        let subject = Subject { account_id: 1, role, class_id: None, class_mask: 0, grant_mask: 0, revoke_mask: 0 };

        let mut eval = PermissionEvaluator::new();
        for (res, principal, rule) in &rules {
            eval.insert_rule(res, *principal, *rule);
        }
        let before = eval.effective(&subject, &query);

        // Strengthen the deny on the most specific level (the query itself).
        eval.insert_rule(&query, Principal::Everyone, AclRule { allow: 0, deny: deny_bits });
        let after = eval.effective(&subject, &query);

        // The denied bits must be absent afterwards.
        prop_assert_eq!(after & deny_bits, 0);
        // And nothing new may appear that wasn't reachable before, except
        // via the level now winning over a farther ancestor — in which case
        // allows are 0 here, so `after ⊆ before ∪ base`.
        let base = subject.base_caps();
        prop_assert_eq!(after & !(before | base), 0);
    }
}
