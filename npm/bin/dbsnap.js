#!/usr/bin/env node
"use strict";

// Run the binary bundled for this operating system and architecture.
const { spawn } = require("child_process");
const path = require("path");
const fs = require("fs");

function fail(message) {
  console.error("dbsnap: " + message);
  process.exit(1);
}

const targets = {
  "win32-x64": "x86_64-pc-windows-msvc",
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-arm64": "aarch64-unknown-linux-gnu",
};
const platform = `${process.platform}-${process.arch}`;
let target = targets[platform];
if (platform === "linux-x64") {
  const report = process.report && process.report.getReport();
  target = report && report.header.glibcVersionRuntime
    ? "x86_64-unknown-linux-gnu"
    : "x86_64-unknown-linux-musl";
}
if (!target) fail(`no bundled binary for ${platform}`);
const binary = path.join(__dirname, "..", "binaries", target,
  process.platform === "win32" ? "dbsnap.exe" : "dbsnap");
if (!fs.existsSync(binary)) fail(`bundled binary is missing for ${target}; reinstall @farhan0431/db-snap`);
const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
child.on("error", (err) => fail(`failed to launch ${binary}: ${err.message}`));
child.on("close", (code) => process.exit(code ?? 1));
