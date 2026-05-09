from __future__ import annotations

import unittest

from db import Table
from ingest import insert_customer
from migrations import add_required_tier
from reports import high_value_customers, revenue_by_tier


class SchemaDefaultsTests(unittest.TestCase):
    def test_migration_backfills_existing_rows(self) -> None:
        table = Table(
            [
                {"customer_id": "c1", "spend": 120},
                {"customer_id": "c2", "spend": 40},
            ]
        )

        add_required_tier(table)

        self.assertEqual(revenue_by_tier(table), {"standard": 160})

    def test_new_rows_still_use_explicit_tier(self) -> None:
        table = Table([{"customer_id": "c1", "spend": 120}])
        add_required_tier(table)

        insert_customer(table, "c2", 60, tier="enterprise")

        self.assertEqual(revenue_by_tier(table), {"standard": 120, "enterprise": 60})

    def test_required_tier_is_enforced_after_migration(self) -> None:
        table = Table([{"customer_id": "c1", "spend": 120}])
        add_required_tier(table)

        with self.assertRaises(ValueError):
            table.insert({"customer_id": "c2", "spend": 10})

    def test_unrelated_report_keeps_existing_behavior(self) -> None:
        table = Table(
            [
                {"customer_id": "c1", "spend": 120},
                {"customer_id": "c2", "spend": 40},
            ]
        )
        add_required_tier(table)

        self.assertEqual(high_value_customers(table), ["c1"])


if __name__ == "__main__":
    unittest.main()
