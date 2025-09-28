"""Compatibility utilities for third-party dependencies."""

from __future__ import annotations

import sys
import types
from importlib import metadata

def _ensure_torchvision_functional_tensor_module() -> None:
    """Restore ``torchvision.transforms.functional_tensor`` for newer torchvision."""

    module_name = "torchvision.transforms.functional_tensor"

    if module_name in sys.modules:
        return

    try:
        import torchvision.transforms as transforms  # type: ignore
    except ModuleNotFoundError:
        return

    functional = getattr(transforms, "functional", None)
    if functional is None:
        return

    rgb_to_grayscale = getattr(functional, "rgb_to_grayscale", None)
    if rgb_to_grayscale is None:
        return

    module = types.ModuleType(module_name)
    module.__dict__.update({
        "__all__": ["rgb_to_grayscale"],
        "rgb_to_grayscale": rgb_to_grayscale,
    })

    sys.modules[module_name] = module
    setattr(transforms, "functional_tensor", module)


def ensure_basicsr_version_module() -> None:
    """Provide ``basicsr.version`` when the PyPI package omits it."""

    _ensure_torchvision_functional_tensor_module()

    try:
        import basicsr  # type: ignore
    except ModuleNotFoundError:
        return

    if "basicsr.version" in sys.modules:
        return

    try:
        import basicsr.version  # type: ignore  # noqa: F401
        return
    except ModuleNotFoundError:
        pass

    dist_version = None
    for dist_name in ("basicsr", "basicsr-fixed", "my-basicsr"):
        try:
            dist_version = metadata.version(dist_name)
            break
        except Exception:
            continue

    if dist_version is None:
        dist_version = getattr(basicsr, "__version__", "0.0.0")

    gitsha = getattr(basicsr, "__gitsha__", "unknown")

    module = types.ModuleType("basicsr.version")
    module.__dict__.update({
        "__all__": ["__gitsha__", "__version__"],
        "__gitsha__": gitsha,
        "__package__": "basicsr",
        "__version__": dist_version,
    })

    sys.modules["basicsr.version"] = module
    setattr(basicsr, "__version__", dist_version)
    setattr(basicsr, "__gitsha__", gitsha)
    setattr(basicsr, "version", module)

__all__ = ["ensure_basicsr_version_module"]
