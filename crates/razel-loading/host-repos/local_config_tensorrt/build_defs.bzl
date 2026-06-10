# razel host materialization of @local_config_tensorrt//:build_defs.bzl — generator tpl
# (xla/third_party/tensorrt/build_defs.bzl.tpl), NO-TENSORRT fill (tensorrt_configure.bzl:129).
def if_tensorrt(if_true, if_false=[]):
    """Tests whether TensorRT was enabled during the configure process."""
    return if_false

def if_tensorrt_exec(if_true, if_false=[]):
    """Synonym for if_tensorrt."""
    return if_false
