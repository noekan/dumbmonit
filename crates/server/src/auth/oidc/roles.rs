//! Du groupe au rôle.
//!
//! La règle tient en une phrase : membre d'un des groupes listés → `admin`, sinon
//! `viewer`. Sans liste, le fournisseur ne dit rien du rôle et l'on garde celui
//! que le compte a déjà.

use serde_json::Value;

use crate::auth::users::Role;

/// Rôle déduit des groupes annoncés, ou `None` si aucun groupe n'est configuré
/// pour décider.
pub fn role_for_groups(groups: &[String], admin_groups: &[String]) -> Option<Role> {
    if admin_groups.is_empty() {
        return None;
    }
    let admin = groups.iter().any(|group| admin_groups.iter().any(|wanted| wanted == group));
    Some(if admin { Role::Admin } else { Role::Viewer })
}

/// Lit la revendication de groupes, quelle que soit sa forme : tableau de chaînes
/// (le cas normal), chaîne unique, ou chaîne séparée par des virgules ou des
/// espaces comme certains fournisseurs la renvoient.
pub fn groups_from_claim(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => {
            items.iter().filter_map(Value::as_str).map(str::to_string).collect()
        }
        Some(Value::String(text)) => text
            .split([',', ' '])
            .map(str::trim)
            .filter(|group| !group.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_member_of_a_listed_group_is_admin_and_the_others_are_viewers() {
        let admin_groups = strings(&["dumbmonit-admins", "ops"]);
        assert_eq!(role_for_groups(&strings(&["dev", "ops"]), &admin_groups), Some(Role::Admin));
        assert_eq!(role_for_groups(&strings(&["dev"]), &admin_groups), Some(Role::Viewer));
        assert_eq!(role_for_groups(&[], &admin_groups), Some(Role::Viewer));
    }

    #[test]
    fn without_a_configured_list_the_provider_says_nothing() {
        assert_eq!(role_for_groups(&strings(&["ops"]), &[]), None);
    }

    #[test]
    fn the_groups_claim_is_read_in_its_common_shapes() {
        assert_eq!(groups_from_claim(Some(&json!(["a", "b", 3]))), strings(&["a", "b"]));
        assert_eq!(groups_from_claim(Some(&json!("a, b c"))), strings(&["a", "b", "c"]));
        assert!(groups_from_claim(None).is_empty());
        assert!(groups_from_claim(Some(&json!(42))).is_empty());
    }
}
