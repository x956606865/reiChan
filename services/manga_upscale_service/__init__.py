"""Public exports for the manga upscaling service package."""

from ._compat import ensure_basicsr_version_module

ensure_basicsr_version_module()

from . import executor  # re-export for tests and downstream modules

__all__ = ["executor"]
