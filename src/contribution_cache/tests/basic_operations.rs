//! Basic ContributionCache operations tests

use std::path::PathBuf;

use super::super::{
    CompactMessageStats, ContributionCache, MultiSessionContribution, PathHash,
    SingleMessageContribution, SingleSessionContribution,
};
use crate::types::CompactDate;

// ============================================================================
// ContributionCache Basic Operations Tests
// ============================================================================

#[test]
fn test_contribution_cache_single_message_insert_get() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/file1.json");
    let path_hash = PathHash::new(&path);

    let contrib = SingleMessageContribution {
        stats: CompactMessageStats {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        date: CompactDate::from_str("2025-01-15").unwrap(),
        model: None,
        session_hash: 12345,
    };

    cache.insert_single_message(path_hash, contrib);
    let retrieved = cache.get_single_message(&path_hash);

    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.stats.input_tokens, 100);
    assert_eq!(retrieved.stats.output_tokens, 50);
    assert_eq!(retrieved.session_hash, 12345);
}

#[test]
fn test_contribution_cache_single_session_insert_get() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/session1.jsonl");
    let path_hash = PathHash::new(&path);

    let contrib = SingleSessionContribution {
        stats: Default::default(),
        date: CompactDate::from_str("2025-01-15").unwrap(),
        models: crate::types::ModelCounts::new(),
        session_hash: 67890,
        ai_message_count: 5,
    };

    cache.insert_single_session(path_hash, contrib);
    let retrieved = cache.get_single_session(&path_hash);

    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.ai_message_count, 5);
}

#[test]
fn test_contribution_cache_multi_session_insert_get() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/app.db");
    let path_hash = PathHash::new(&path);

    let contrib = MultiSessionContribution {
        session_aggregates: vec![],
        daily_stats: Default::default(),
        conversation_count: 10,
    };

    cache.insert_multi_session(path_hash, contrib);
    let retrieved = cache.get_multi_session(&path_hash);

    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().conversation_count, 10);
}

#[test]
fn test_contribution_cache_remove_any() {
    let cache = ContributionCache::new();

    // Insert one of each type
    let path1 = PathBuf::from("/test/msg.json");
    let path2 = PathBuf::from("/test/session.jsonl");
    let path3 = PathBuf::from("/test/app.db");

    let hash1 = PathHash::new(&path1);
    let hash2 = PathHash::new(&path2);
    let hash3 = PathHash::new(&path3);

    cache.insert_single_message(
        hash1,
        SingleMessageContribution {
            stats: Default::default(),
            date: Default::default(),
            model: None,
            session_hash: 1,
        },
    );
    cache.insert_single_session(
        hash2,
        SingleSessionContribution {
            stats: Default::default(),
            date: Default::default(),
            models: crate::types::ModelCounts::new(),
            session_hash: 2,
            ai_message_count: 0,
        },
    );
    cache.insert_multi_session(
        hash3,
        MultiSessionContribution {
            session_aggregates: vec![],
            daily_stats: Default::default(),
            conversation_count: 3,
        },
    );

    // Remove and verify correct type returned
    let removed1 = cache.remove_any(&hash1);
    assert!(matches!(
        removed1,
        Some(super::super::RemovedContribution::SingleMessage(_))
    ));

    let removed2 = cache.remove_any(&hash2);
    assert!(matches!(
        removed2,
        Some(super::super::RemovedContribution::SingleSession(_))
    ));

    let removed3 = cache.remove_any(&hash3);
    assert!(matches!(
        removed3,
        Some(super::super::RemovedContribution::MultiSession(_))
    ));

    // Verify they're actually removed
    assert!(cache.get_single_message(&hash1).is_none());
    assert!(cache.get_single_session(&hash2).is_none());
    assert!(cache.get_multi_session(&hash3).is_none());
}

#[test]
fn test_contribution_cache_clear() {
    let cache = ContributionCache::new();

    let path = PathBuf::from("/test/file.json");
    let hash = PathHash::new(&path);

    cache.insert_single_message(
        hash,
        SingleMessageContribution {
            stats: Default::default(),
            date: Default::default(),
            model: None,
            session_hash: 1,
        },
    );

    assert!(cache.get_single_message(&hash).is_some());

    cache.clear();

    assert!(cache.get_single_message(&hash).is_none());
}

// ============================================================================
// Utility Tests
// ============================================================================

#[test]
fn test_path_hash_consistency() {
    let path1 = PathBuf::from("/test/file.json");
    let path2 = PathBuf::from("/test/file.json");
    let path3 = PathBuf::from("/test/other.json");

    let hash1 = PathHash::new(&path1);
    let hash2 = PathHash::new(&path2);
    let hash3 = PathHash::new(&path3);

    assert_eq!(hash1, hash2, "Same paths should have same hash");
    assert_ne!(hash1, hash3, "Different paths should have different hash");
}
