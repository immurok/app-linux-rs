//! Substring filter for the quick-fill list. Pure so it can be tested.

use immurok_client::keys::KeyEntry;

/// Case-insensitive substring match on name and service. Empty query keeps
/// everything. Order of `entries` is preserved.
pub fn filter_entries(entries: &[KeyEntry], query: &str) -> Vec<KeyEntry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|e| e.name.to_lowercase().contains(&q) || e.service.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use immurok_client::keys::KeyCategory;

    fn e(name: &str, service: &str) -> KeyEntry {
        KeyEntry {
            index: 0,
            category: KeyCategory::Otp,
            name: name.into(),
            service: service.into(),
            ssh_pubkey_b64: String::new(),
        }
    }

    #[test]
    fn empty_query_keeps_all() {
        let all = vec![e("aws", "Amazon"), e("gh", "GitHub")];
        assert_eq!(filter_entries(&all, "  ").len(), 2);
    }

    #[test]
    fn matches_name_or_service_case_insensitively() {
        let all = vec![e("aws", "Amazon"), e("gh", "GitHub")];
        assert_eq!(filter_entries(&all, "HUB").len(), 1);
        assert_eq!(filter_entries(&all, "AWS")[0].name, "aws");
        assert!(filter_entries(&all, "zzz").is_empty());
    }
}
