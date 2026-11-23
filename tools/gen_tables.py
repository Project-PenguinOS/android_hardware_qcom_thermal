#!/usr/bin/env python3
#
# SPDX-FileCopyrightText: The LineageOS Project
# SPDX-License-Identifier: Apache-2.0
#
"""Generates tables.rs from the QTI C++ thermal HAL's thermalConfig.cpp.

The Rust HAL keeps the per-SoC sensor tables of the C++ HAL it replaced. After merging a new CLO
thermal tag, regenerate them from that tag's C++ source:

    git show <tag>:thermalConfig.cpp | tools/gen_tables.py --source <tag> - > tables.rs
"""

import argparse
import re
import sys

SEVERITIES = ["NONE", "LIGHT", "MODERATE", "SEVERE", "CRITICAL", "EMERGENCY", "SHUTDOWN"]

TOKEN = re.compile(
    r"""\s*(?:
        (?P<string>"(?:[^"\\]|\\.)*")
      | (?P<number>-?(?:0[xX][0-9a-fA-F]+|\d+))
      | (?P<ident>[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)
      | (?P<punct>[{}\[\]=,;()<>*&])
      | (?P<other>\S)
    )""",
    re.VERBOSE,
)


def strip_comments(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    text = re.sub(r"//[^\n]*", " ", text)
    return re.sub(r"^\s*#[^\n]*", " ", text, flags=re.M)


def tokenize(text):
    tokens = []
    pos = 0
    text = text.rstrip()
    while pos < len(text):
        m = TOKEN.match(text, pos)
        if not m:
            raise SyntaxError(f"cannot tokenize near: {text[pos:pos + 60]!r}")
        kind = m.lastgroup
        value = m.group(kind)
        tokens.append((kind, value))
        pos = m.end()
    return tokens


class Parser:
    def __init__(self, tokens):
        self.tokens = tokens
        self.pos = 0

    def peek(self, offset=0):
        i = self.pos + offset
        return self.tokens[i] if i < len(self.tokens) else (None, None)

    def take(self, value=None):
        token = self.tokens[self.pos]
        if value is not None and token[1] != value:
            raise SyntaxError(f"expected {value!r}, got {token[1]!r} at token {self.pos}")
        self.pos += 1
        return token

    def initializer(self):
        """A brace initializer as a nested list; designators become ('designated', key, value)."""
        self.take("{")
        items = []
        while self.peek()[1] != "}":
            if self.peek()[1] == "[":
                self.take("[")
                key = self.take()[1]
                self.take("]")
                self.take("=")
                items.append(("designated", key, self.value()))
            else:
                items.append(self.value())
            if self.peek()[1] == ",":
                self.take(",")
        self.take("}")
        return items

    def value(self):
        kind, value = self.peek()
        if value == "{":
            return self.initializer()
        if value == "std::make_pair":
            self.take()
            self.take("(")
            first = self.value()
            self.take(",")
            second = self.value()
            self.take(")")
            return ("pair", first, second)
        self.take()
        if kind == "string":
            return ("string", bytes(value[1:-1], "utf-8").decode("unicode_escape"))
        if kind == "number":
            return ("number", int(value, 0))
        return ("ident", value)


def parse_definitions(text):
    """Every `<type> name = <initializer>;` at any nesting, as name -> (type, initializer)."""
    tokens = tokenize(strip_comments(text))
    defs = {}
    i = 0
    while i < len(tokens):
        if tokens[i][1] == "=" and i > 0 and tokens[i - 1][0] == "ident" and i + 1 < len(tokens) \
                and tokens[i + 1][1] == "{":
            name = tokens[i - 1][1]
            # The declared type: tokens back to the previous ';', '{' or '}'.
            j = i - 2
            while j >= 0 and tokens[j][1] not in (";", "{", "}"):
                j -= 1
            decl = " ".join(t[1] for t in tokens[j + 1:i - 1])
            parser = Parser(tokens)
            parser.pos = i + 1
            defs[name] = (decl, parser.initializer())
            i = parser.pos
        else:
            i += 1
    return defs


def string_list(value, strings):
    if isinstance(value, tuple) and value[0] == "ident":
        return strings[value[1]]
    return [item[1] for item in value]


def sensor(entry, strings):
    """One target_therm_cfg initializer."""
    if len(entry) not in (5, 6):
        raise SyntaxError(f"unexpected target_therm_cfg: {entry!r}")
    kind = entry[0][1].split("::")[-1]
    zones = string_list(entry[1], strings)
    label = entry[2][1]
    thresholds = [0] * len(SEVERITIES)
    for item in entry[3]:
        if item[0] != "designated":
            raise SyntaxError(f"positional threshold in {entry!r}")
        thresholds[SEVERITIES.index(item[1])] = item[2][1]
    def flag(v):
        return v[1] == "true" if v[0] == "ident" else bool(v[1])
    positive = flag(entry[4])
    no_trip = flag(entry[5]) if len(entry) == 6 else False
    return kind, zones, label, thresholds, positive, no_trip


def rust_sensor(s):
    kind, zones, label, thresholds, positive, no_trip = s
    zone_list = ", ".join(f'"{z}"' for z in zones)
    return (
        f"    S {{ kind: TemperatureType::{kind}, zones: &[{zone_list}], label: \"{label}\", "
        f"thresholds: {thresholds}, positive: {str(positive).lower()}, "
        f"no_trip: {str(no_trip).lower()} }},"
    )


def first_wins(pairs, what, warnings):
    """unordered_map initializer semantics: a repeated key keeps its first value."""
    seen = {}
    for key, value in pairs:
        if key in seen:
            if seen[key] != value:
                warnings.append(f"{what}: soc {key} listed again as {value}, keeping {seen[key]}")
            continue
        seen[key] = value
    return list(seen.items())


def generate(text, source):
    defs = parse_definitions(text)
    strings = {n: [v[1] for v in init] for n, (d, init) in defs.items()
               if d == "std::vector < std::string >"}
    tables = {}
    for name, (decl, init) in defs.items():
        if decl == "std::vector < struct target_therm_cfg >":
            tables[name] = [sensor(entry, strings) for entry in init]
        elif decl == "struct target_therm_cfg":
            tables[name] = [sensor(init, strings)]

    def pairs(name):
        return [(item[0][1], item[1][1]) for item in defs[name][1]]

    warnings = []
    soc_map = first_wins(pairs("msm_soc_map"), "msm_soc_map", warnings)
    soc_specific = first_wins(pairs("msm_soc_specific"), "msm_soc_specific", warnings)
    profiles = [(item[0][1], item[1][1][1], item[1][2][1])
                for item in defs["msm_limit_profile_specific"][1]]
    platforms = [(item[0][1], item[1][1][1], item[1][2][1])
                 for item in defs["msm_platform_specific"][1]]
    no_bcl = [item[0][1] for item in defs["batt_bcl_not_supported"][1]]

    used = {t for _, t in soc_map} | {t for _, t in soc_specific} | \
        {t for _, _, t in profiles} | {t for _, _, t in platforms} | {"bat_conf", "bcl_conf"}
    missing = used - tables.keys()
    if missing:
        raise SyntaxError(f"tables referenced but not defined: {sorted(missing)}")

    out = []
    out.append("""/*
 * Copyright (c) 2020, The Linux Foundation. All rights reserved.
 * Changes from Qualcomm Innovation Center are provided under the following license:
 * Copyright (c) 2023 Qualcomm Innovation Center, Inc. All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause-Clear
 */

// @generated by tools/gen_tables.py from the QTI thermalConfig.cpp""")
    out.append(f"// of {source}. Do not edit; regenerate instead.")
    out.append("""
//! The QTI per-SoC sensor tables, in the lookup order of the C++ HAL.

use crate::config::StaticSensor as S;
use android_hardware_thermal::aidl::android::hardware::thermal::TemperatureType::TemperatureType;
""")
    for name in sorted(used):
        out.append(f"pub const {name.upper()}: &[S] = &[")
        out.extend(rust_sensor(s) for s in tables[name])
        out.append("];\n")

    out.append("/// msm_soc_map: the base config of each SoC.")
    out.append("pub const SOC_MAP: &[(i32, &[S])] = &[")
    out.extend(f"    ({soc}, {t.upper()})," for soc, t in soc_map)
    out.append("];\n")
    out.append("/// msm_soc_specific: added to the base config.")
    out.append("pub const SOC_SPECIFIC: &[(i32, &[S])] = &[")
    out.extend(f"    ({soc}, {t.upper()})," for soc, t in soc_specific)
    out.append("];\n")
    out.append("/// msm_limit_profile_specific: (soc, limits profile, config), added for that profile.")
    out.append("pub const LIMIT_PROFILE_SPECIFIC: &[(i32, i32, &[S])] = &[")
    out.extend(f"    ({soc}, {lp}, {t.upper()})," for soc, lp, t in profiles)
    out.append("];\n")
    out.append("/// msm_platform_specific: (soc, /sys/devices/soc0/hw_platform, config).")
    out.append("pub const PLATFORM_SPECIFIC: &[(i32, &str, &[S])] = &[")
    out.extend(f'    ({soc}, "{hw}", {t.upper()}),' for soc, hw, t in platforms)
    out.append("];\n")
    out.append("/// batt_bcl_not_supported: SoCs without the battery and BCL configs.")
    out.append(f"pub const BATT_BCL_NOT_SUPPORTED: &[i32] = &{no_bcl};")
    return "\n".join(out) + "\n", warnings


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("cpp", help="thermalConfig.cpp, or - for stdin")
    parser.add_argument("--source", required=True, help="the CLO tag or commit it came from")
    args = parser.parse_args()
    text = sys.stdin.read() if args.cpp == "-" else open(args.cpp).read()
    rust, warnings = generate(text, args.source)
    for w in warnings:
        print(f"warning: {w}", file=sys.stderr)
    sys.stdout.write(rust)


if __name__ == "__main__":
    main()
