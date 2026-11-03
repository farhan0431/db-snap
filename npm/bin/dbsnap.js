#!/usr/bin/env node
"use strict";

// Launcher for the dbsnap Rust binary. The npm meta package "db-snap"
// declares per-platform packages (db-snap-<platform>-<arch>) as
// optionalDependencies; npm installs only the matching one and this script
// finds and execs it, passing stdin/stdout through untouched (required for
// MCP mode).

const { spawn } = require("child_process");
const path = require("path");
const fs = require("fs");

const PKGS_BY_TARGET = {
  "win32-x64": ["@farhan0431/db-snap-win32-x64"],
  "darwin-arm64": ["db-snap-darwin-arm64"],
  "darwin-x64": ["db-snap-darwin-x64"],
  "linux-x64": ["db-snap-linux-x64-gnu", "db-snap-linux-x64-musl"],
  "linux-arm64": ["db-snap-linux-arm64"],
};

function fail(msg) {
  console.error("dbsnap: " + msg);
  process.exit(1);
}

function resolveBinary() {
  const target = `${process.platform}-${process.arch}`;
  const candidates = PKGS_BY_TARGET[target];
  if (!candidates) {
    fail(
      `no prebuilt binary for ${target}. Supported: ${Object.keys(PKGS_BY_TARGET).join(", ")}. ` +
        `Build from source: cargo install --git https://github.com/farhan0431/db-snap`
    );
  }
  for (const pkg of candidates) {
    try {
      const pkgJson = require.resolve(path.join(pkg, "package.json"), {
        paths: [__dirname, process.cwd()],
      });
      const dir = path.dirname(pkgJson);
      const bin = path.join(dir, "bin", process.platform === "win32" ? "dbsnap.exe" : "dbsnap");
      if (fs.existsSync(bin)) return bin;
    } catch {
      // try the next candidate (e.g. musl fallback)
    }
  }
  fail(
    `binary for ${target} is not installed. Reinstall with: npm i -g @farhan0431/db-snap ` +
      `(if you installed with --no-optional / --ignore-scripts the platform package was skipped)`
  );
}

const bin = resolveBinary();
const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
child.on("error", (err) => fail(`failed to launch ${bin}: ${err.message}`));
child.on("close", (code) => process.exit(code ?? 1));
