//! Memory storage implementation tests using shared test functions

use mdk_memory_storage::MdkMemoryStorage;

mod shared;

/// Macro to generate tests that run against Memory storage using shared test functions
macro_rules! test_memory_storage {
    ($test_name:ident, $test_fn:path) => {
        #[test]
        fn $test_name() {
            let storage = MdkMemoryStorage::default();
            $test_fn(storage);
        }
    };
}

// Group functionality tests
test_memory_storage!(
    test_save_and_find_group_memory,
    shared::group_tests::test_save_and_find_group
);

test_memory_storage!(test_all_groups_memory, shared::group_tests::test_all_groups);

test_memory_storage!(
    test_group_exporter_secret_memory,
    shared::group_tests::test_group_exporter_secret
);

test_memory_storage!(
    test_basic_group_relays_memory,
    shared::group_tests::test_basic_group_relays
);

test_memory_storage!(
    test_group_edge_cases_memory,
    shared::group_tests::test_group_edge_cases
);

test_memory_storage!(
    test_replace_relays_edge_cases_memory,
    shared::group_tests::test_replace_relays_edge_cases
);

// Comprehensive relay tests
test_memory_storage!(
    test_replace_group_relays_comprehensive_memory,
    shared::group_tests::test_replace_group_relays_comprehensive
);

test_memory_storage!(
    test_replace_group_relays_error_cases_memory,
    shared::group_tests::test_replace_group_relays_error_cases
);

test_memory_storage!(
    test_replace_group_relays_duplicate_handling_memory,
    shared::group_tests::test_replace_group_relays_duplicate_handling
);

// Admin functionality tests
test_memory_storage!(test_admins_memory, shared::group_tests::test_admins);

test_memory_storage!(
    test_admins_error_for_nonexistent_group_memory,
    shared::group_tests::test_admins_error_for_nonexistent_group
);

// Message functionality tests
test_memory_storage!(
    test_save_and_find_message_memory,
    shared::message_tests::test_save_and_find_message
);

test_memory_storage!(
    test_processed_message_memory,
    shared::message_tests::test_processed_message
);

test_memory_storage!(
    test_messages_for_group_memory,
    shared::group_tests::test_messages_for_group
);

test_memory_storage!(
    test_messages_error_for_nonexistent_group_memory,
    shared::group_tests::test_messages_error_for_nonexistent_group
);

test_memory_storage!(
    test_group_relays_error_for_nonexistent_group_memory,
    shared::group_tests::test_group_relays_error_for_nonexistent_group
);

test_memory_storage!(
    test_messages_sort_order_memory,
    shared::group_tests::test_messages_sort_order
);

test_memory_storage!(
    test_messages_sort_order_pagination_memory,
    shared::group_tests::test_messages_sort_order_pagination
);

test_memory_storage!(
    test_last_message_memory,
    shared::group_tests::test_last_message
);

test_memory_storage!(
    test_groups_needing_self_update_memory,
    shared::group_tests::test_groups_needing_self_update
);

// Welcome functionality tests
test_memory_storage!(
    test_save_and_find_welcome_memory,
    shared::welcome_tests::test_save_and_find_welcome
);

test_memory_storage!(
    test_processed_welcome_memory,
    shared::welcome_tests::test_processed_welcome
);
