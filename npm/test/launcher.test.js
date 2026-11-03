"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");
const vm = require("vm");

const launcher = fs.readFileSync(path.join(__dirname, "../bin/dbsnap.js"), "utf8");
const cases = [
  ["win32", "x64", null, "x86_64-pc-windows-msvc"],
  ["darwin", "arm64", null, "aarch64-apple-darwin"],
  ["darwin", "x64", null, "x86_64-apple-darwin"],
  ["linux", "arm64", "2.39", "aarch64-unknown-linux-gnu"],
  ["linux", "x64", "2.39", "x86_64-unknown-linux-gnu"],
  ["linux", "x64", null, "x86_64-unknown-linux-musl"],
];

for (const [platform, arch, glibc, expected] of cases) {
  let selected;
  const processMock = {
    platform, arch, argv: ["node", "dbsnap", "--version"],
    report: { getReport: () => ({ header: { glibcVersionRuntime: glibc } }) },
    exit: () => { throw new Error("unexpected exit"); },
  };
  const requireMock = (name) => {
    if (name === "child_process") return { spawn: (file, args) => {
      selected = file;
      assert.deepStrictEqual(Array.from(args), ["--version"]);
      return { on: () => {} };
    } };
    if (name === "fs") return { existsSync: () => true };
    return require(name);
  };
  vm.runInNewContext(launcher, {
    require: requireMock, process: processMock, __dirname: "/package/bin", console,
  });
  assert(selected.includes(expected), `${platform}-${arch} selected ${selected}`);
}
console.log("Launcher selects all six platform binaries");
