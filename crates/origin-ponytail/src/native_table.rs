// SPDX-License-Identifier: Apache-2.0
//! Ported platform-native dependency table (from ponytail's platform-native.md).
//! Only package→native rows are included; only deps with a genuine stdlib/native
//! replacement appear. Packages that earn their place (`ms`, `requests`, `click`,
//! `httparty`, `rand`, `itertools`, `react`, …) are deliberately omitted.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    Npm,
    PyPI,
    Cargo,
    Go,
    RubyGems,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeReplacement {
    /// 2 = standard library, 3 = native platform feature.
    pub rung: u8,
    pub native: &'static str,
    pub note: &'static str,
}

const fn r(rung: u8, native: &'static str, note: &'static str) -> NativeReplacement {
    NativeReplacement { rung, native, note }
}

const NPM: &[(&str, NativeReplacement)] = &[
    ("query-string", r(3, "new URLSearchParams(location.search)", "0 deps")),
    ("qs", r(3, "new URLSearchParams(...)", "0 deps")),
    ("lodash.clonedeep", r(3, "structuredClone(obj)", "native")),
    ("lodash.groupby", r(3, "Object.groupBy(arr, fn)", "native")),
    ("lodash", r(3, "native Array/Object methods (groupBy, structuredClone, …)", "drop the umbrella dep")),
    ("moment", r(3, "Intl.DateTimeFormat / Temporal", "native i18n dates")),
    ("date-fns", r(3, "Intl.DateTimeFormat / Intl.RelativeTimeFormat", "native")),
    ("numeral", r(3, "new Intl.NumberFormat(...)", "native")),
    ("accounting", r(3, "new Intl.NumberFormat(..., {style:'currency'})", "native")),
    ("clipboard.js", r(3, "navigator.clipboard.writeText(text)", "native")),
    ("uuid", r(3, "crypto.randomUUID()", "native, v4")),
    ("uuid-validate", r(3, "/^[0-9a-f]{8}-...$/i.test(id)", "1-line regex")),
    ("left-pad", r(2, "String.prototype.padStart(n, '0')", "stdlib")),
    ("is-online", r(3, "navigator.onLine + online/offline events", "native")),
    ("mkdirp", r(2, "fs.mkdirSync(path, { recursive: true })", "stdlib")),
    ("make-dir", r(2, "fs.mkdirSync(path, { recursive: true })", "stdlib")),
    ("rimraf", r(2, "fs.rmSync(path, { recursive: true, force: true })", "stdlib")),
    ("slash", r(2, "path.posix / path.normalize()", "stdlib")),
    ("is-stream", r(2, "val instanceof stream.Readable", "stdlib")),
    ("object-assign", r(2, "Object.assign() / spread", "stdlib")),
    ("array-uniq", r(2, "[...new Set(arr)]", "stdlib")),
    ("array-flatten", r(2, "arr.flat(Infinity)", "stdlib")),
    ("flat", r(2, "arr.flat(depth)", "stdlib")),
    ("path-exists", r(2, "fs.existsSync(path)", "stdlib")),
    ("load-json-file", r(2, "JSON.parse(fs.readFileSync(path, 'utf8'))", "stdlib")),
    ("write-json-file", r(2, "fs.writeFileSync(path, JSON.stringify(obj, null, 2))", "stdlib")),
    ("pkg-dir", r(2, "path.resolve(__dirname, '..')", "stdlib")),
];

const PYPI: &[(&str, NativeReplacement)] = &[
    ("python-dateutil", r(2, "datetime.fromisoformat()", "stdlib 3.7+")),
    ("pytz", r(2, "zoneinfo.ZoneInfo", "stdlib 3.9+")),
    ("attrs", r(2, "@dataclass", "stdlib")),
    ("six", r(2, "(drop it — Python 2 is gone)", "stdlib")),
    ("pathlib2", r(2, "pathlib.Path", "stdlib 3.4+")),
    ("enum34", r(2, "enum.Enum", "stdlib 3.4+")),
    ("typing_extensions", r(2, "builtin generics + from __future__ import annotations", "stdlib")),
    ("simplejson", r(2, "json", "stdlib")),
    ("mergedeep", r(2, "dict | other_dict", "stdlib 3.9+")),
    ("more-itertools", r(2, "itertools (chain, islice, groupby, product)", "stdlib")),
    ("toolz", r(2, "functools (lru_cache, partial, reduce)", "stdlib")),
    ("tabulate", r(2, "pprint.pprint()", "stdlib, debug only")),
];

const CARGO: &[(&str, NativeReplacement)] = &[
    ("lazy_static", r(2, "std::sync::LazyLock", "stdlib 1.80")),
    ("once_cell", r(2, "std::sync::OnceLock / LazyLock", "stdlib")),
    ("num_cpus", r(2, "std::thread::available_parallelism()", "stdlib 1.59")),
    ("maplit", r(2, "HashMap::from([...]) / BTreeMap::from", "stdlib 1.56")),
    ("failure", r(2, "std::error::Error (+ thiserror/anyhow)", "stdlib")),
    ("error-chain", r(2, "std::error::Error", "stdlib")),
];

const GO: &[(&str, NativeReplacement)] = &[
    ("github.com/pkg/errors", r(2, "errors + fmt.Errorf(\"%w\")", "stdlib 1.13")),
    ("github.com/sirupsen/logrus", r(2, "log/slog", "stdlib 1.21")),
    ("github.com/gorilla/mux", r(3, "net/http.ServeMux (method+wildcard)", "stdlib 1.22")),
    ("golang.org/x/exp/slices", r(2, "slices", "stdlib 1.21")),
    ("golang.org/x/exp/maps", r(2, "maps", "stdlib 1.21")),
];

const RUBYGEMS: &[(&str, NativeReplacement)] = &[
    ("rest-client", r(2, "net/http", "stdlib; gem unmaintained")),
    ("awesome_print", r(2, "pp", "stdlib, debug")),
];

#[must_use]
pub fn lookup(eco: Ecosystem, pkg: &str) -> Option<&'static NativeReplacement> {
    let mut name = pkg.trim().to_ascii_lowercase();
    if eco == Ecosystem::Cargo {
        name = name.replace('-', "_"); // crates.io treats - and _ interchangeably
    }
    let table = match eco {
        Ecosystem::Npm => NPM,
        Ecosystem::PyPI => PYPI,
        Ecosystem::Cargo => CARGO,
        Ecosystem::Go => GO,
        Ecosystem::RubyGems => RUBYGEMS,
    };
    table.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_replacements_hit() {
        assert_eq!(lookup(Ecosystem::Npm, "lodash.groupby").unwrap().native, "Object.groupBy(arr, fn)");
        assert_eq!(lookup(Ecosystem::PyPI, "pytz").unwrap().native, "zoneinfo.ZoneInfo");
        assert_eq!(lookup(Ecosystem::Cargo, "lazy_static").unwrap().rung, 2);
        // hyphen/underscore normalization for cargo
        assert!(lookup(Ecosystem::Cargo, "lazy-static").is_some());
        assert_eq!(lookup(Ecosystem::Go, "github.com/pkg/errors").unwrap().native, "errors + fmt.Errorf(\"%w\")");
    }

    #[test]
    fn omitted_packages_miss() {
        // Genuinely-useful packages are deliberately absent (never gated).
        assert!(lookup(Ecosystem::Npm, "ms").is_none());
        assert!(lookup(Ecosystem::PyPI, "requests").is_none());
        assert!(lookup(Ecosystem::PyPI, "click").is_none());
        assert!(lookup(Ecosystem::RubyGems, "httparty").is_none());
        assert!(lookup(Ecosystem::Npm, "react").is_none());
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup(Ecosystem::Npm, "Lodash.GroupBy").is_some());
    }
}
