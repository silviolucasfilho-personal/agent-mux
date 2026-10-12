#!/usr/bin/env python3
"""Refresh curated model rates in pricing.toml from LiteLLM's catalog."""

import argparse
import datetime as dt
import json
import math
import re
import sys
from pathlib import Path


PRICE_FIELDS = {
    "input": "input_cost_per_token",
    "output": "output_cost_per_token",
    "cache_read": "cache_read_input_token_cost",
    "cache_write": "cache_creation_input_token_cost",
    "cache_write_1h": "cache_creation_input_token_cost_above_1hr",
    "reasoning": "output_cost_per_reasoning_token",
}
PRICE_FIELD_ORDER = list(PRICE_FIELDS)
OPTIONAL_FIELDS = set(PRICE_FIELDS) - {"input", "output"}
BLOCK_RE = re.compile(r"(?m)^\[\[models\]\]\s*$")


def _model_id(block):
    match = re.search(r'(?m)^id = "([^"\n]+)"$', block)
    if not match:
        raise ValueError("model block has no id")
    return match.group(1)


def _find_upstream(model_id, catalog):
    if model_id in catalog:
        return catalog[model_id]
    dated = sorted(key for key in catalog if key.startswith(model_id + "-20") or key.startswith(model_id + "@"))
    if dated:
        return catalog[dated[0]]
    for prefix in ("anthropic.", "vertex_ai/", "azure/", "gemini/", "openai/"):
        key = prefix + model_id
        if key in catalog:
            return catalog[key]
    return None


def _rates(entry):
    if not isinstance(entry, dict):
        return None
    rates = {}
    for field, upstream_name in PRICE_FIELDS.items():
        value = entry.get(upstream_name)
        if value is None:
            continue
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return None
        value = float(value) * 1_000_000
        if not math.isfinite(value) or value < 0:
            return None
        if field in OPTIONAL_FIELDS and value == 0:
            continue
        rates[field] = value
    if "input" not in rates or "output" not in rates:
        return None
    reasoning = rates.get("reasoning")
    if reasoning is not None and abs(reasoning - rates["output"]) <= 1e-9:
        rates.pop("reasoning")
    return rates


def _format_number(value):
    formatted = format(value, ".12g")
    if "." not in formatted and "e" not in formatted:
        formatted += ".0"
    return formatted


def _set_field(block, name, value):
    pattern = re.compile(r"(?m)^" + re.escape(name) + r" = [^\n]*\n?")
    if value is None:
        return pattern.sub("", block)
    replacement = "{} = {}\n".format(name, _format_number(value))
    if pattern.search(block):
        return pattern.sub(replacement, block, count=1)
    # Keep price fields in their declared order after the curated model metadata.
    lines = block.splitlines(keepends=True)
    insert_at = len(lines)
    for index, line in enumerate(lines):
        match = re.match(r"([a-z_]+) = ", line)
        if match and match.group(1) in PRICE_FIELD_ORDER:
            if PRICE_FIELD_ORDER.index(match.group(1)) > PRICE_FIELD_ORDER.index(name):
                insert_at = index
                break
            insert_at = index + 1
    while insert_at > 0 and not lines[insert_at - 1].strip():
        insert_at -= 1
    lines.insert(insert_at, replacement)
    return "".join(lines)


def _update_block(block, catalog):
    model_id = _model_id(block)
    entry = _find_upstream(model_id, catalog)
    rates = _rates(entry)
    if rates is None:
        return block, False

    updated = re.sub(
        r"(?m)^# kept from the previous table \(not in LiteLLM\)\n", "", block
    )
    for field in PRICE_FIELDS:
        updated = _set_field(updated, field, rates.get(field))
    return updated, updated != block


def update_table(table_text, source_path, as_of=None):
    """Return (updated TOML, whether any curated rates changed)."""
    try:
        catalog = json.loads(Path(source_path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError("could not read LiteLLM pricing JSON: {}".format(error))
    if not isinstance(catalog, dict) or not any(_rates(entry) for entry in catalog.values()):
        raise ValueError("LiteLLM pricing JSON contains no usable model prices")

    blocks = BLOCK_RE.split(table_text)
    if len(blocks) == 1:
        raise ValueError("pricing table contains no [[models]] rows")
    prefix = blocks[0]
    updated_blocks = []
    changed = False
    for block in blocks[1:]:
        updated, block_changed = _update_block(block, catalog)
        updated_blocks.append(updated)
        changed = changed or block_changed

    if not changed:
        return table_text, False

    result = "[[models]]".join([prefix] + updated_blocks)
    date_value = as_of or dt.datetime.now(dt.timezone.utc).date().isoformat()
    try:
        dt.date.fromisoformat(date_value)
    except ValueError:
        raise ValueError("--as-of must be an ISO date (YYYY-MM-DD)")
    result, count = re.subn(
        r'(?m)^updated_at = "[^"\n]+"$',
        'updated_at = "{}"'.format(date_value),
        result,
        count=1,
    )
    if count != 1:
        raise ValueError("pricing table has no updated_at date")
    return result, True


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--source", required=True, type=Path, help="downloaded LiteLLM pricing JSON"
    )
    parser.add_argument("--table", type=Path, default=Path("src/tracing/pricing.toml"))
    parser.add_argument("--as-of", help="snapshot date (defaults to the current UTC date)")
    args = parser.parse_args(argv)
    try:
        original = args.table.read_text(encoding="utf-8")
        updated, changed = update_table(original, args.source, args.as_of)
        if changed:
            args.table.write_text(updated, encoding="utf-8")
            print("Updated model prices in {}".format(args.table))
        else:
            print("No curated model prices changed")
    except (OSError, ValueError) as error:
        print("error: {}".format(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
