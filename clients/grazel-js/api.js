"use strict";
// GENERATED native JS types + codec — do not edit. Pairs with cbor.js.
const { CInt, CText, CBytes, CBool, CArr, CMap, CNull, cget, cmapEntries, isNull } = require("./cbor.js");

const TargetKind = Object.freeze({ library: 0, binary: 1, test: 2 });

const BuildStatus = Object.freeze({ cached: 0, built: 1, failed: 2 });

class OutputArtifact {
  constructor(o = {}) {
    this.path = o.path;
    this.digest = o.digest;
  }
  toCbor() {
    const m = [
      [1, CText(this.path)],
      [2, CBytes(this.digest)],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new OutputArtifact();
    v.path = cget(c, 1).s;
    v.digest = cget(c, 2).b;
    return v;
  }
}

class TargetRef {
  constructor(o = {}) {
    this.label = o.label;
    this.kind = o.kind;
  }
  toCbor() {
    const m = [
      [1, CText(this.label)],
      [2, CInt(this.kind)],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new TargetRef();
    v.label = cget(c, 1).s;
    v.kind = cget(c, 2).i;
    return v;
  }
}

class BuildResult {
  constructor(o = {}) {
    this.target = o.target;
    this.status = o.status;
    this.recomputes = o.recomputes;
    this.outputs = o.outputs;
    this.message = o.message;
  }
  toCbor() {
    const m = [
      [1, CText(this.target)],
      [2, CInt(this.status)],
      [3, CInt(this.recomputes)],
      [4, CArr(this.outputs.map((e) => e.toCbor()))],
      [5, (this.message != null ? CText(this.message) : CNull())],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new BuildResult();
    v.target = cget(c, 1).s;
    v.status = cget(c, 2).i;
    v.recomputes = cget(c, 3).i;
    v.outputs = cget(c, 4).arr.map((e) => OutputArtifact.fromCbor(e));
    { const f = cget(c, 5); v.message = isNull(f) ? null : f.s; }
    return v;
  }
}

class SyncAck {
  constructor(o = {}) {
    this.revision = o.revision;
  }
  toCbor() {
    const m = [
      [1, CInt(this.revision)],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new SyncAck();
    v.revision = cget(c, 1).i;
    return v;
  }
}

class VersionInfo {
  constructor(o = {}) {
    this.version = o.version;
    this.protocol = o.protocol;
  }
  toCbor() {
    const m = [
      [1, CText(this.version)],
      [2, CInt(this.protocol)],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new VersionInfo();
    v.version = cget(c, 1).s;
    v.protocol = cget(c, 2).i;
    return v;
  }
}

class InvocationStarted {
  constructor(o = {}) {
    this.invocation_id = o.invocation_id;
  }
  toCbor() {
    const m = [
      [1, CText(this.invocation_id)],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new InvocationStarted();
    v.invocation_id = cget(c, 1).s;
    return v;
  }
}

class Progress {
  constructor(o = {}) {
    this.invocation_id = o.invocation_id;
    this.phase = o.phase;
    this.done = o.done;
    this.total = o.total;
    this.detail = o.detail;
  }
  toCbor() {
    const m = [
      [1, CText(this.invocation_id)],
      [2, CText(this.phase)],
      [3, CInt(this.done)],
      [4, CInt(this.total)],
      [5, (this.detail != null ? CText(this.detail) : CNull())],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new Progress();
    v.invocation_id = cget(c, 1).s;
    v.phase = cget(c, 2).s;
    v.done = cget(c, 3).i;
    v.total = cget(c, 4).i;
    { const f = cget(c, 5); v.detail = isNull(f) ? null : f.s; }
    return v;
  }
}

class InvocationEvent {
  constructor(o = {}) {
    this.invocation_id = o.invocation_id;
    this.seq = o.seq;
    this.progress = o.progress;
    this.result = o.result;
  }
  toCbor() {
    const m = [
      [1, CText(this.invocation_id)],
      [2, CInt(this.seq)],
      [3, (this.progress != null ? this.progress.toCbor() : CNull())],
      [4, (this.result != null ? this.result.toCbor() : CNull())],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new InvocationEvent();
    v.invocation_id = cget(c, 1).s;
    v.seq = cget(c, 2).i;
    { const f = cget(c, 3); v.progress = isNull(f) ? null : Progress.fromCbor(f); }
    { const f = cget(c, 4); v.result = isNull(f) ? null : BuildResult.fromCbor(f); }
    return v;
  }
}

class Hello {
  constructor(o = {}) {
    this.build_version = o.build_version;
    this.protocol = o.protocol;
    this.workspace_root = o.workspace_root;
  }
  toCbor() {
    const m = [
      [1, CText(this.build_version)],
      [2, CInt(this.protocol)],
      [3, CText(this.workspace_root)],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new Hello();
    v.build_version = cget(c, 1).s;
    v.protocol = cget(c, 2).i;
    v.workspace_root = cget(c, 3).s;
    return v;
  }
}

class ImpactSet {
  constructor(o = {}) {
    this.sources = o.sources;
    this.targets = o.targets;
    this.tests = o.tests;
  }
  toCbor() {
    const m = [
      [1, CArr(this.sources.map((e) => CText(e)))],
      [2, CArr(this.targets.map((e) => e.toCbor()))],
      [3, CArr(this.tests.map((e) => e.toCbor()))],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new ImpactSet();
    v.sources = cget(c, 1).arr.map((e) => e.s);
    v.targets = cget(c, 2).arr.map((e) => TargetRef.fromCbor(e));
    v.tests = cget(c, 3).arr.map((e) => TargetRef.fromCbor(e));
    return v;
  }
}

class TargetStatus {
  constructor(o = {}) {
    this.label = o.label;
    this.kind = o.kind;
    this.status = o.status;
    this.output_digest = o.output_digest;
  }
  toCbor() {
    const m = [
      [1, CText(this.label)],
      [2, CInt(this.kind)],
      [3, CInt(this.status)],
      [4, CBytes(this.output_digest)],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new TargetStatus();
    v.label = cget(c, 1).s;
    v.kind = cget(c, 2).i;
    v.status = cget(c, 3).i;
    v.output_digest = cget(c, 4).b;
    return v;
  }
}

class BuildState {
  constructor(o = {}) {
    this.revision = o.revision;
    this.targets = o.targets;
  }
  toCbor() {
    const m = [
      [1, CInt(this.revision)],
      [2, CArr(this.targets.map((e) => e.toCbor()))],
    ];
    return CMap(m);
  }
  static fromCbor(c) {
    const v = new BuildState();
    v.revision = cget(c, 1).i;
    v.targets = cget(c, 2).arr.map((e) => TargetStatus.fromCbor(e));
    return v;
  }
}

module.exports = { TargetKind, BuildStatus, OutputArtifact, TargetRef, BuildResult, SyncAck, VersionInfo, InvocationStarted, Progress, InvocationEvent, Hello, ImpactSet, TargetStatus, BuildState };
