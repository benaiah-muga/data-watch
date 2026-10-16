"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");

const root = path.resolve(__dirname, "..");
const read = (name) => fs.readFileSync(path.join(root, name), "utf8");
const manifest = read("Cargo.toml");
const readme = read("README.md");
const license = read("LICENSE");
const main = read("src/main.rs");
assert(manifest.includes('license = "Apache-2.0"'), "Cargo metadata does not declare Apache-2.0");
assert(license.includes("Apache License") && license.includes("Version 2.0, January 2004"), "Apache-2.0 license text is missing");
assert(readme.includes("My phone's hotspot report showed that my PC was using about 80%"), "README introduction is missing the origin story");
assert(readme.includes("Windows and macOS support are future work"), "README does not state the current platform scope");
assert(read("CONTRIBUTING.md").includes("Apache License, Version 2.0"), "contribution terms are missing");
assert(manifest.includes('features = ["bundled"]'), "SQLite is not bundled");
assert(main.includes('include_bytes!("../static/index.html")') && main.includes('include_bytes!("../static/app.js")') && main.includes('include_bytes!("../static/app.css")'), "dashboard assets are not embedded in the binary");
assert(main.includes('include_bytes!("../static/favicon.svg")') && main.includes('include_bytes!("../static/fonts/ibm-plex-sans-latin.woff2")'), "favicon or font is not embedded in the binary");
assert(read("static/fonts/OFL.txt").includes("SIL OPEN FONT LICENSE"), "bundled font license is missing");
assert(main.includes("Icon=data-watch") && main.includes("data-watch.svg"), "installer does not create an app launcher icon");
assert(!main.includes("CARGO_MANIFEST_DIR"), "runtime still depends on the source checkout");
process.stdout.write("OPEN_SOURCE_CHECK_PASS\n");
