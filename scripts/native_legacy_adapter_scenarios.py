# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fixed selected partial legacy adapters; never full-leaf or release parity."""

import ast
import hashlib
import sys
from pathlib import Path

DECLARED_CASES = {
    "tests.test_rust_legacy_destination_mappings.NativeDestinationMappingTest": (
        "test_standard_preview_stdout_and_typed_dto_reports",
        "test_scalar_plan_apply_noop_and_ownership",
        "test_owned_sets_and_removal_budget",
        "test_stale_plan_validation_and_identity",
        "test_uncertain_and_observation_only_recovery",
        "test_lost_response_is_not_retried_and_recovery_verifies",
        "test_recovery_uncertain_and_unsupported_options",
        "test_explicit_collections_and_missing_rows_retain",
        "test_no_catalog_or_sqlite_lock_during_network",
        "test_query_grouping_expressions_and_complete_empty_results",
        "test_truncated_query_and_duplicate_columns_fail_closed",
        "test_other_owner_remote_identity_and_owned_value_loss",
        "test_metadata_lock_contention_precedes_remote_and_releases",
        "test_connection_revision_change_stops_after_durable_first_item",
        "test_query_change_stops_after_durable_first_item",
        "test_recovery_catalog_writer_contention_preserves_event_and_lock",
        "test_real_interrupt_after_write_keeps_started_recoverable",
    ),
    "tests.test_rust_legacy_destination_mappings.NativeJellyfinDestinationMappingTest": (
        "test_numeric_dto_prewrite_fence_matches_frozen",
        "test_numeric_dto_not_applied_recovery_matches_frozen",
    ),
    "tests.test_rust_publication_mapping_preview.PublicationMappingPreviewTest": (
        "test_populated_complete_plan_and_repeat_rollback",
        "test_all_seventeen_presets_use_complete_selection",
        "test_definition_and_option_scalar_admission",
        "test_unknown_query_catalog_and_foreign_projection_errors",
        "test_query_revision_and_definition_options_pin_plan",
        "test_complete_file_association_and_paginated_graphql_selections",
        "test_empty_query_guard_and_allow_empty_plans_rollback",
        "test_pending_history_order_and_other_profile_isolation",
        "test_nonfolder_preview_admission_and_apply_recover_no_effects",
    ),
    "tests.test_rust_publication_bundle_preview.PublicationBundlePreviewTest": (
        "test_three_export_formats_preserve_complete_plan_and_repeat",
        "test_nfo_ignores_unused_base_url_but_definition_stays_in_identity",
        "test_calibre_aliases_preserve_groups_and_destination_identity",
        "test_import_submitted_history_dedup_and_revision_guard",
        "test_import_destination_identity_and_untransmitted_metadata",
        "test_selection_and_import_unsupported_association",
        "test_export_existing_foreign_destination_and_preflight_collision",
        "test_export_verified_bundle_history_and_changed_artifact",
        "test_pending_history_and_empty_selection",
        "test_apply_and_recovery_still_refuse_before_bundle_effects",
        "test_immich_identity_scope_dedup_and_account_change",
        "test_immich_legacy_history_refuses_before_identity_request",
        "test_immich_invalid_identity_status_and_redirect_errors",
        "test_import_multiformat_groups_and_duplicate_format_refusal",
        "test_document_limit_and_source_revision_dedup_refusal",
        "test_immich_cancellation_stops_read_only_helper",
    ),
    "tests.test_rust_legacy_external_rule.NativeLegacyExternalRuleTest": (
        "test_external_definition_revisions_idempotency_and_typed_options",
        "test_external_definition_validation_has_no_catalog_effects",
        "test_external_configuration_utf8_byte_boundary",
        "test_empty_selection_is_complete_without_claiming_or_writes",
        "test_matching_jobs_must_be_registered_with_this_rule",
        "test_batch_and_details_are_independent_and_unselected_jobs_untouched",
        "test_pending_same_worker_polls_once_without_new_generation",
        "test_partial_batch_stops_at_first_dispatch_error_and_preserves_evidence",
        "test_failed_response_is_a_durable_step_and_does_not_stop_later_jobs",
        "test_capacity_reports_no_claim_and_never_steals_unrelated_lease",
        "test_expired_registered_lease_is_reclaimed_and_old_attempt_retained",
        "test_worker_admission_errors_never_dispatch_or_write",
        "test_empty_worker_uses_normal_pending_report_without_network",
        "test_missing_credential_is_durable_dispatch_error_without_network",
        "test_invalid_state_and_receipt_refuse_before_any_publication",
        "test_network_wait_releases_application_and_sqlite_writer_locks",
        "test_cancellation_during_network_wait_fences_late_response",
        "test_interrupt_reaps_http_child_and_leaves_reclaimable_durable_lease",
    ),
    "tests.test_rust_legacy_mapping_history.NativeLegacyMappingHistoryTest": (
        "test_empty_history_unknown_events_and_profiles",
        "test_combined_local_authority_profile_order_and_complete_events",
        "test_last_hundred_and_http_thousand_cap_before_event_lookup",
        "test_publication_only_last_hundred",
        "test_http_queue_states_options_status_and_apply_refusal",
        "test_apply_metadata_lock_precedence_and_readonly_catalog_authority",
        "test_duplicate_aggregate_event_first_lookup_but_recovery_rejected",
        "test_publication_version_two_stays_explicitly_unavailable",
        "test_missing_database_admission",
        "test_arbitrary_stored_subject_order_is_preserved",
        "test_readonly_interruption_catalog_and_lock_cleanup",
    ),
    "tests.test_rust_legacy_mapping_event_order.NativeLegacyMappingEventOrderTest": (
        "test_nested_permutations_report_like_objects_and_scalar_values",
        "test_duplicate_keys_keep_first_position_and_last_value",
        "test_http_shaped_publication_uses_actual_source_and_first_aggregate_match",
        "test_exit_code_and_empty_containers_follow_authoritative_value",
        "test_numeric_token_normalization_uses_owner_values",
        "test_real_sigint_discards_selected_sidecar_and_releases_snapshot",
    ),
    "tests.test_rust_legacy_external_worker_diagnostics.ExternalWorkerDiagnosticsTest": (
        "test_redirect_status_and_location_presence_for_put_and_get",
        "test_ordinary_location_forms_and_legacy_uri_header",
        "test_other_statuses_keep_their_diagnostic_and_success_neighbors",
        "test_exact_response_byte_boundaries_with_length_and_chunked_bodies",
        "test_valid_receipt_after_durable_cancellation_remains_rejected",
        "test_resumed_poll_cleanup_neighbor",
        "test_writer_release_cleanup_neighbor",
        "test_cancel_cleanup_neighbor",
        "test_interrupt_cleanup_neighbor",
    ),
}
INHERITED = {
    "tests.test_rust_legacy_destination_mappings.NativeJellyfinDestinationMappingTest": "tests.test_rust_legacy_destination_mappings.NativeDestinationMappingTest",
    "tests.test_rust_legacy_external_worker_diagnostics.ExternalWorkerDiagnosticsTest": "tests.test_rust_legacy_external_rule.NativeLegacyExternalRuleTest",
}
# The diagnostic module intentionally selects only its nine own methods. Its
# complete inherited inventory is still checked, without rerunning the base 18.
OWN_ONLY = (
    "tests.test_rust_legacy_external_worker_diagnostics.ExternalWorkerDiagnosticsTest",
)
SOURCE_BASES = {
    OWN_ONLY[0]: "fixture.NativeLegacyExternalRuleTest",
}
REVIEWED_LOAD_TESTS = {
    "tests.test_rust_legacy_external_worker_diagnostics": ast.dump(
        ast.parse(
            "def load_tests(_loader, _tests, _pattern):\n"
            "    names = sorted(n for n in ExternalWorkerDiagnosticsTest.__dict__ if n.startswith('test_'))\n"
            "    return unittest.TestSuite(ExternalWorkerDiagnosticsTest(n) for n in names)\n"
        ).body[0]
    ),
}
REQUIRED_CASES = {
    name: (() if name in OWN_ONLY else DECLARED_CASES.get(INHERITED.get(name), ()))
    + methods
    for name, methods in DECLARED_CASES.items()
}
REQUIRED_IDS = tuple(
    f"{name}.{method}" for name, methods in REQUIRED_CASES.items() for method in methods
)
REQUIRED_COUNT = 105
# These selected methods include deliberate unavailable-path witnesses, not
# evidence of Python/native parity for publication apply or recovery.
NONPARITY_WITNESSES = (
    "tests.test_rust_legacy_mapping_history.NativeLegacyMappingHistoryTest.test_publication_version_two_stays_explicitly_unavailable",
    "tests.test_rust_publication_bundle_preview.PublicationBundlePreviewTest.test_apply_and_recovery_still_refuse_before_bundle_effects",
    "tests.test_rust_publication_mapping_preview.PublicationMappingPreviewTest.test_nonfolder_preview_admission_and_apply_recover_no_effects",
)
NATIVE_ONLY_WITNESSES = (
    "tests.test_rust_legacy_mapping_history.NativeLegacyMappingHistoryTest.test_publication_version_two_stays_explicitly_unavailable",
    "tests.test_rust_publication_bundle_preview.PublicationBundlePreviewTest.test_immich_cancellation_stops_read_only_helper",
)
SUPPORT_MODULES = ("tests.test_rust_legacy_projection",)
MODULES = tuple(dict.fromkeys(name.rpartition(".")[0] for name in REQUIRED_CASES))
LOAD_MODULES = SUPPORT_MODULES + MODULES
FIXTURE_FILES = ("tests/__init__.py", "tests/test_rust_legacy_review.py") + tuple(
    module.replace(".", "/") + ".py" for module in LOAD_MODULES
)


def fixture_imports(tree):
    """Resolve direct test-package aliases as well as explicit module imports."""
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            yield from (item.name for item in node.names)
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                raise RuntimeError("relative legacy fixture imports are not declared")
            if node.module == "tests":
                yield from ("tests." + item.name for item in node.names)
            else:
                yield node.module


def validate_sources(root):
    """Keep duplicates, inheritance, imports, and newly declared tests visible."""
    if len(REQUIRED_IDS) != REQUIRED_COUNT or len(set(REQUIRED_IDS)) != REQUIRED_COUNT:
        raise RuntimeError("legacy adapter inventory is not exact and distinct")
    hashes = {}
    declared = {}
    for relative in FIXTURE_FILES:
        path = Path(root) / relative
        if path.is_symlink() or not path.is_file():
            raise RuntimeError(f"missing or linked legacy fixture: {relative}")
        data = path.read_bytes()
        hashes[relative] = hashlib.sha256(data).hexdigest()
        module = relative.removesuffix(".py").replace("/", ".")
        tree = ast.parse(data, filename=str(path))
        for imported in fixture_imports(tree):
            if imported.partition(".")[0] == "tests":
                if imported.replace(".", "/") + ".py" not in FIXTURE_FILES:
                    raise RuntimeError(f"undeclared legacy fixture import: {imported}")
            elif imported.partition(".")[0] not in sys.stdlib_module_names:
                raise RuntimeError(f"non-stdlib legacy fixture import: {imported}")
        if module not in MODULES:
            continue
        accepted = set()
        for node in tree.body:
            if not isinstance(node, ast.ClassDef):
                continue
            name = module + "." + node.name
            methods = [
                item
                for item in node.body
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
                and item.name.startswith("test_")
            ]
            if name not in DECLARED_CASES:
                if methods or any(ast.unparse(base) != "object" for base in node.bases):
                    raise RuntimeError(f"unexpected legacy test class: {name}")
                continue
            if name in declared or node.decorator_list or node.keywords:
                raise RuntimeError(f"duplicate or decorated legacy test class: {name}")
            expected_base = SOURCE_BASES.get(name) or (
                INHERITED.get(name, "unittest.TestCase").rpartition(".")[2]
                if name in INHERITED
                else "unittest.TestCase"
            )
            if [ast.unparse(base) for base in node.bases] != [expected_base]:
                raise RuntimeError(f"legacy test inheritance drift: {name}")
            declared[name] = tuple(item.name for item in methods)
            accepted.update(map(id, methods))
            if sorted(declared[name]) != sorted(DECLARED_CASES[name]):
                raise RuntimeError(f"legacy declared method inventory drift: {name}")
        routing = [
            node
            for node in ast.walk(tree)
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name == "load_tests"
        ]
        expected_route = REVIEWED_LOAD_TESTS.get(module)
        if [ast.dump(node) for node in routing] != (
            [expected_route] if expected_route else []
        ):
            raise RuntimeError(f"unexpected legacy load_tests routing: {relative}")
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                if node.name.startswith("test_") and id(node) not in accepted:
                    raise RuntimeError(f"unexpected legacy test routing: {relative}")
    if set(declared) != set(DECLARED_CASES):
        raise RuntimeError("missing legacy test classes")
    return hashes
