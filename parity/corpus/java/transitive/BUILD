load("@rules_java//java:defs.bzl", "java_library")

# Corpus case: transitive java (util → base). Mirrors cc/transitive — exercises the JavaCompile
# action and transitive classpath propagation (util compiles against base's jar), the java baseline
# (RazelParityHarness.md §9). The second instance that stress-tests the razel_build abstraction.

java_library(
    name = "base",
    srcs = ["Base.java"],
)

java_library(
    name = "util",
    srcs = ["Util.java"],
    deps = [":base"],
)
