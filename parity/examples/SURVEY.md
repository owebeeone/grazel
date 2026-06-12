# Examples survey — the burn-down (generated: `cargo xtask examples --survey`)

Frontier view: load+analyze per workspace (the curated goldens corpus in
examples.rs owns the per-verb columns for its members). Re-run to refresh.

| workspace | pkgs | green | first error |
|---|---|---|---|
| android/firebase-cloud-messaging | 2 | 1 | `app`: error: unsupported load path `@rules_android//android:rules.bzl` (only //pkg:f.bzl, :f.bzl |
| android/jetpack-compose | 2 | 0 | ``: error: unsupported load path `@io_bazel_rules_kotlin//kotlin:core.bzl` (only //pkg:f.bzl,  |
| android/ndk | 2 | 1 | `app/src/main`: error: unsupported load path `@rules_android//android:rules.bzl` (only //pkg:f.bzl, :f.bzl |
| android/robolectric-testing | 2 | 1 | `app`: error: unsupported load path `@io_bazel_rules_kotlin//kotlin:android.bzl` (only //pkg:f.bz |
| bzlmod/01-depend_on_bazel_module | 1 | 0 | ``: loading `@com_google_absl//absl/log:log`'s package failed: external repo for `@com_google_ |
| bzlmod/02-override_bazel_module | 1 | 0 | ``: error: Module has no symbol `hello_msg` |
| bzlmod/02-override_bazel_module/lib_a | 1 | 0 | ``: loading `@com_google_absl//absl/log:log`'s package failed: external repo for `@com_google_ |
| bzlmod/03-introduce_dependencies_with_module_extension | 1 | 1 | — |
| bzlmod/03-introduce_dependencies_with_module_extension/lib_a | 1 | 1 | — |
| bzlmod/04-local_config_and_register_toolchains | 1 | 0 | ``: Traceback (most recent call last): |
| bzlmod/05-integrate_third_party_package_manager | 1 | 1 | — |
| bzlmod/05-integrate_third_party_package_manager/lib_a | 1 | 1 | — |
| bzlmod/05-integrate_third_party_package_manager/lib_b | 1 | 1 | — |
| bzlmod/06-specify_dev_dependency | 1 | 0 | ``: error: Module has no symbol `version` |
| bzlmod/06-specify_dev_dependency/lib_a | 1 | 1 | — |
| bzlmod/utils/librarian | 1 | 1 | — |
| configurations | 10 | 2 | `attaching_transitions_to_rules`: error: Module has no symbol `BuildSettingInfo` |
| configurations/auto_configured_builds | 7 | 7 | — |
| configurations/auto_configured_builds/custom_flags_impl | 1 | 1 | — |
| configurations/cc_test | 1 | 0 | ``: Traceback (most recent call last): |
| cpp-tutorial/stage1 | 1 | 1 | — |
| cpp-tutorial/stage2 | 1 | 1 | — |
| cpp-tutorial/stage3 | 2 | 2 | — |
| flags-parsing-tutorial | 1 | 1 | — |
| frontend | 21 | 5 | ``: error: unsupported load path `@npm//:defs.bzl` (only //pkg:f.bzl, :f.bzl, or a vendored @r |
| go-tutorial/stage1 | 1 | 0 | ``: error: unsupported load path `@rules_go//go:def.bzl` (only //pkg:f.bzl, :f.bzl, or a vendo |
| go-tutorial/stage2 | 2 | 0 | ``: error: unsupported load path `@rules_go//go:def.bzl` (only //pkg:f.bzl, :f.bzl, or a vendo |
| go-tutorial/stage3 | 2 | 0 | ``: error: unsupported load path `@rules_go//go:def.bzl` (only //pkg:f.bzl, :f.bzl, or a vendo |
| java-maven | 1 | 0 | ``: error: unsupported load path `@aspect_bazel_lib//lib:tar.bzl` (only //pkg:f.bzl, :f.bzl, o |
| java-tutorial | 2 | 1 | `src/main/java/com/example/cmdline`: `//:greeter` is neither a declared target nor a source file in this package |
| macros | 6 | 5 | `main`: error: Variable `macro` not found |
| make-variables | 1 | 0 | `testapp`: $(FOO) is not a modeled genrule Make variable (razel models SRCS/OUTS/location/locations) |
| query-quickstart | 6 | 6 | — |
| rules | 19 | 14 | `attributes`: Traceback (most recent call last): |
| rust-examples/01-hello-world | 1 | 1 | — |
| rust-examples/02-hello-cross | 3 | 3 | — |
| rust-examples/03-comp-opt | 2 | 2 | — |
| rust-examples/04-ffi | 2 | 2 | — |
| rust-examples/05-deps-cargo | 1 | 0 | ``: error: unsupported load path `@crates//:defs.bzl` (only //pkg:f.bzl, :f.bzl, or a vendored |
| rust-examples/06-deps-direct | 2 | 1 | `rest_tokio`: loading `@crates//:arc-swap`'s package failed: external repo for `@crates//` not vendored |
| rust-examples/07-deps-vendor | 3 | 1 | `basic`: loading `//thirdparty/crates:tokio`'s package failed: no BUILD in package `thirdparty/crat |
| rust-examples/08-grpc-client-server | 6 | 1 | `build/prost_toolchain`: error: unsupported load path `@rules_rust_prost//:defs.bzl` (only //pkg:f.bzl, :f.bzl, or  |
| rust-examples/09-oci-container | 2 | 1 | `tokio_oci`: error: unsupported load path `@rules_oci//oci:defs.bzl` (only //pkg:f.bzl, :f.bzl, or a ve |
| third-party-dependencies | 2 | 1 | ``: error: unsupported load path `@buildifier_prebuilt//:rules.bzl` (only //pkg:f.bzl, :f.bzl, |
