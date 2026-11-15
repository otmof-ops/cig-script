// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/startup-10.cig: start-up with ten stdlib calls. Each line is
// the call a Node program makes for the same thing, and the requires are the ones
// those calls need.
const path = require("path");
const crypto = require("crypto");

const _1 = Math.abs(-1);
const _2 = String(1);
const _3 = JSON.stringify({ a: 1 });
const _4 = "a b".split(" ");
const _5 = path.basename("/x/y");
const _6 = new Date(0).toISOString();
const _7 = crypto.createHash("sha256").update("x").digest("hex").slice(0, 8);
const _8 = process.version;
const _9 = process.env.PATH ?? "";
const _10 = [].length;
console.log("hello");
