"""Unit tests for the content-aware double page splitter prototype."""

from __future__ import annotations

from pathlib import Path

import cv2
import numpy as np
import pytest

from manga_upscale_service.doublepage_split import SplitConfig, SplitResult, split_image


@pytest.fixture
def sample_config() -> SplitConfig:
    """Provide a configuration instance with defaults suitable for tests."""

    return SplitConfig(
        min_aspect_ratio=1.2,
        padding_ratio=0.02,
        confidence_threshold=0.1,
        cover_content_ratio=0.45,
        edge_exclusion_ratio=0.12,
    )


def _make_canvas(width: int, height: int) -> np.ndarray:
    """Create a white canvas with the given width/height."""

    canvas = np.full((height, width, 3), 255, dtype=np.uint8)
    return canvas


def test_split_detects_double_page_and_outputs_right_first(sample_config: SplitConfig) -> None:
    image = _make_canvas(800, 400)

    # Draw two dense panels separated by a narrow gutter near the center.
    cv2.rectangle(image, (40, 40), (360, 360), (0, 0, 0), thickness=-1)
    cv2.rectangle(image, (440, 40), (760, 360), (0, 0, 0), thickness=-1)

    result = split_image(image, config=sample_config)

    assert result.mode == "split"
    assert result.split_x is not None
    assert 360 <= result.split_x <= 460

    assert len(result.pages) == 2
    right, left = result.pages

    # Right page comes first for RTL ordering and retains foreground pixels.
    assert np.mean(right[:, -20:]) < 240  # near the outer edge should contain content
    assert np.mean(left[:, :20]) < 240


def test_split_identifies_cover_and_trims_padding(sample_config: SplitConfig) -> None:
    image = _make_canvas(900, 420)

    # Central narrow cover artwork occupying ~30% width but full height.
    cv2.rectangle(image, (370, 40), (530, 380), (30, 30, 30), thickness=-1)

    result = split_image(image, config=sample_config)

    assert result.mode == "cover-trim"
    assert result.split_x is None
    assert len(result.pages) == 1

    trimmed = result.pages[0]
    assert trimmed.shape[1] < image.shape[1] * 0.6
    assert trimmed.shape[0] >= image.shape[0] * 0.75


def test_split_low_confidence_falls_back_to_center(sample_config: SplitConfig) -> None:
    image = _make_canvas(820, 400)
    cv2.rectangle(image, (40, 40), (780, 360), (0, 0, 0), thickness=-1)

    result = split_image(image, config=sample_config)

    assert result.mode == "fallback-center"
    assert result.split_x == image.shape[1] // 2
    assert len(result.pages) == 2

    left = result.pages[1]
    right = result.pages[0]

    # Fallback keeps both halves roughly same width.
    assert abs(left.shape[1] - right.shape[1]) <= 4


def test_split_skip_when_aspect_ratio_below_threshold(sample_config: SplitConfig) -> None:
    image = _make_canvas(400, 400)
    cv2.rectangle(image, (40, 40), (360, 360), (0, 0, 0), thickness=-1)

    result = split_image(image, config=sample_config)

    assert result.mode == "skip"
    assert result.pages == []

