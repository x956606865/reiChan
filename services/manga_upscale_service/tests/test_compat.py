"""Tests for compatibility helpers."""

from __future__ import annotations

import sys
import types
from importlib import metadata

import pytest

from manga_upscale_service._compat import ensure_basicsr_version_module


@pytest.fixture(autouse=True)
def cleanup_modules():
    snapshot = dict(sys.modules)
    yield
    for name in list(sys.modules):
        if name not in snapshot:
            sys.modules.pop(name, None)
    for name, module in snapshot.items():
        sys.modules[name] = module


def test_ensure_basicsr_version_module_creates_shim(monkeypatch):
    basicsr = types.ModuleType("basicsr")
    monkeypatch.setitem(sys.modules, "basicsr", basicsr)
    sys.modules.pop("basicsr.version", None)

    def fake_version(name: str) -> str:
        if name == "basicsr":
            raise metadata.PackageNotFoundError(name)
        if name == "basicsr-fixed":
            return "1.4.2"
        raise AssertionError(f"unexpected distribution {name}")

    monkeypatch.setattr(
        "manga_upscale_service._compat.metadata.version",
        fake_version,
    )

    ensure_basicsr_version_module()

    module = sys.modules.get("basicsr.version")
    assert module is not None
    assert getattr(module, "__version__") == "1.4.2"
    assert getattr(module, "__gitsha__") == "unknown"


def test_ensure_basicsr_version_module_backfills_torchvision_tensor(monkeypatch):
    torchvision = types.ModuleType("torchvision")
    transforms = types.ModuleType("torchvision.transforms")
    functional = types.ModuleType("torchvision.transforms.functional")

    def rgb_to_grayscale(image, *args, **kwargs):
        return ("converted", image, args, tuple(sorted(kwargs.items())))

    functional.rgb_to_grayscale = rgb_to_grayscale
    transforms.functional = functional
    torchvision.transforms = transforms

    monkeypatch.setitem(sys.modules, "torchvision", torchvision)
    monkeypatch.setitem(sys.modules, "torchvision.transforms", transforms)
    monkeypatch.setitem(sys.modules, "torchvision.transforms.functional", functional)
    sys.modules.pop("torchvision.transforms.functional_tensor", None)

    basicsr = types.ModuleType("basicsr")
    monkeypatch.setitem(sys.modules, "basicsr", basicsr)
    monkeypatch.setattr(
        "manga_upscale_service._compat.metadata.version",
        lambda name: "1.4.2" if name in {"basicsr", "basicsr-fixed", "my-basicsr"} else "0.0",
    )

    ensure_basicsr_version_module()

    module = sys.modules.get("torchvision.transforms.functional_tensor")
    assert module is not None
    assert getattr(module, "rgb_to_grayscale") is rgb_to_grayscale
    assert getattr(transforms, "functional_tensor") is module
