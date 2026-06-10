# razel host materialization of @proto_bazel_features (generated — template:
# com_google_protobuf/bazel/private/oss/proto_bazel_features.bzl). Modern-host fills
# (starlark ProtoInfo era; razel-as-host >= Bazel 8 posture: ProtoInfo/cc_proto_aspect = None
# i.e. provided by the Starlark protobuf rules themselves).
bazel_features = struct(
    cc = struct(
        protobuf_on_allowlist = True,
    ),
    proto = struct(
        starlark_proto_info = True,
    ),
    rules = struct(
        analysis_tests_can_transition_on_experimental_incompatible_flags = True,
    ),
    globals = struct(
        PackageSpecificationInfo = PackageSpecificationInfo,
        ProtoInfo = None,
        cc_proto_aspect = None,
    ),
)
