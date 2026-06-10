# razel host materialization of @python_version_repo (a configure-GENERATED repo — template:
# rules_ml_toolchain/py/python_repo.bzl). Defaults = the generator's no-env defaults
# (USE_PYWRAP_RULES False = TF's legacy path; hermetic python 3.11).
TF_PYTHON_VERSION = "3.11"
HERMETIC_PYTHON_VERSION = "3.11"
HERMETIC_PYTHON_VERSION_KIND = ""
WHEEL_NAME = "tensorflow"
WHEEL_COLLAB = "False"
REQUIREMENTS = "//:requirements.txt"
REQUIREMENTS_WITH_LOCAL_WHEELS = "//:requirements.txt"
LOCAL_WHEEL_OVERRIDES_LABEL = ""
USE_PYWRAP_RULES = False
MACOSX_DEPLOYMENT_TARGET = "11.0"
HERMETIC_PYTHON_URL = ""
HERMETIC_PYTHON_SHA256 = ""
HERMETIC_PYTHON_PREFIX = ""
