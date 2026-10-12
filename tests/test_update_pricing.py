import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "update_pricing.py"
spec = importlib.util.spec_from_file_location("update_pricing", SCRIPT)
update_pricing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(update_pricing)


class UpdatePricingTests(unittest.TestCase):
    def transform(self, table, upstream, as_of="2026-10-12"):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "upstream.json"
            source.write_text(json.dumps(upstream), encoding="utf-8")
            return update_pricing.update_table(table, source, as_of)

    def test_refreshes_rates_and_keeps_curated_metadata(self):
        table = '''# pricing table\nupdated_at = "2026-10-04"\n\n[[models]]\n# kept from the previous table (not in LiteLLM)\nid = "claude-sonnet-4-5"\nprovider = "anthropic"\nmatch = ["claude-sonnet-4-5", "claude-sonnet-4-5-*"]\ninput = 3.0\noutput = 15.0\ncache_read = 0.3\ncache_write = 3.75\ncache_write_1h = 6.0\n'''
        upstream = {
            "claude-sonnet-4-5-20250929": {
                "input_cost_per_token": 0.000004,
                "output_cost_per_token": 0.00002,
                "cache_read_input_token_cost": 0.0000004,
                "cache_creation_input_token_cost": 0.000005,
                "output_cost_per_reasoning_token": 0.000019,
            }
        }

        updated, changed = self.transform(table, upstream)

        self.assertTrue(changed)
        self.assertIn('updated_at = "2026-10-12"', updated)
        self.assertIn('match = ["claude-sonnet-4-5", "claude-sonnet-4-5-*"]', updated)
        self.assertIn("input = 4.0", updated)
        self.assertIn("output = 20.0", updated)
        self.assertIn("cache_read = 0.4", updated)
        self.assertIn("cache_write = 5.0", updated)
        self.assertIn("reasoning = 19.0", updated)
        self.assertNotIn("cache_write_1h", updated)
        self.assertNotIn("kept from the previous table", updated)

    def test_missing_upstream_model_preserves_existing_rates_and_marker(self):
        table = '''updated_at = "2026-10-04"\n\n[[models]]\n# kept from the previous table (not in LiteLLM)\nid = "local-model"\nprovider = "other"\nmatch = ["local-model"]\ninput = 1.0\noutput = 2.0\n'''

        updated, changed = self.transform(table, {"unrelated": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}})

        self.assertFalse(changed)
        self.assertEqual(updated, table)

    def test_no_price_change_does_not_bump_snapshot_date(self):
        table = '''updated_at = "2026-10-04"\n\n[[models]]\nid = "gpt-5"\nprovider = "openai"\nmatch = ["gpt-5"]\ninput = 1.0\noutput = 2.0\n'''
        upstream = {"gpt-5": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}

        updated, changed = self.transform(table, upstream)

        self.assertFalse(changed)
        self.assertEqual(updated, table)

    def test_reasoning_rate_equal_to_output_is_not_stored_separately(self):
        table = '''updated_at = "2026-10-04"\n\n[[models]]\nid = "gpt-5"\nprovider = "openai"\nmatch = ["gpt-5"]\ninput = 1.0\noutput = 2.0\n'''
        upstream = {
            "gpt-5": {
                "input_cost_per_token": 1e-6,
                "output_cost_per_token": 2e-6,
                "output_cost_per_reasoning_token": 2e-6,
            }
        }

        updated, changed = self.transform(table, upstream)

        self.assertFalse(changed)
        self.assertEqual(updated, table)

    def test_rejects_an_upstream_catalog_with_no_usable_model_prices(self):
        table = '''updated_at = "2026-10-04"\n\n[[models]]\nid = "gpt-5"\nprovider = "openai"\nmatch = ["gpt-5"]\ninput = 1.0\noutput = 2.0\n'''

        with self.assertRaises(ValueError):
            self.transform(table, {"gpt-5": {"input_cost_per_token": "bad"}})


if __name__ == "__main__":
    unittest.main()
