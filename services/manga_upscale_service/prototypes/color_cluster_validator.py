"""Prototype script for validating dynamic threshold + color clustering margins.

Usage example:
    python color_cluster_validator.py --input /path/to/page.png --output-debug debug.png

This script follows the guidance in ``docs/manga-split-dark-margin-strategies.md``
by running K-means on side bands and the gutter band to derive adaptive
thresholds that identify dark margins and the split gutter.
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

import cv2
import numpy as np


@dataclass
class ColorClusterConfig:
    side_band_ratio: float = 0.08
    center_band_ratio: float = 0.04
    sample_step: int = 4
    entropy_bins: int = 32
    entropy_window: int = 15
    k_clusters: int = 2
    std_multiplier: float = 0.75
    background_score_threshold: float = 0.6
    max_center_ratio: float = 0.06
    min_margin_ratio: float = 0.025


@dataclass
class MarginRegion:
    start_x: int
    end_x: int
    mean_score: float
    confidence: float


@dataclass
class BandStats:
    label: str
    mean_L: float
    std_L: float
    entropy_L: float
    weight_score: float
    coverage_ratio: float
    threshold: float


@dataclass
class ClusterResult:
    width: int
    height: int
    left_margin: Optional[MarginRegion]
    right_margin: Optional[MarginRegion]
    center_band: Optional[MarginRegion]
    left_band_stats: Optional[BandStats]
    right_band_stats: Optional[BandStats]
    center_band_stats: Optional[BandStats]

    def to_json(self) -> str:
        payload = {
            "width": self.width,
            "height": self.height,
            "left_margin": asdict(self.left_margin) if self.left_margin else None,
            "right_margin": asdict(self.right_margin) if self.right_margin else None,
            "center_band": asdict(self.center_band) if self.center_band else None,
            "left_band_stats": asdict(self.left_band_stats) if self.left_band_stats else None,
            "right_band_stats": asdict(self.right_band_stats) if self.right_band_stats else None,
            "center_band_stats": asdict(self.center_band_stats) if self.center_band_stats else None,
        }
        return json.dumps(payload, indent=2)


def _load_image(path: Path) -> np.ndarray:
    image = cv2.imread(str(path), cv2.IMREAD_COLOR)
    if image is None:
        raise FileNotFoundError(f"Failed to load image: {path}")
    return image


def _lab_image(image: np.ndarray) -> np.ndarray:
    lab = cv2.cvtColor(image, cv2.COLOR_BGR2Lab)
    return lab.astype(np.float32)


def _entropy_from_values(values: np.ndarray, bins: int) -> float:
    if values.size == 0:
        return 0.0
    hist, _ = np.histogram(values, bins=bins, range=(0.0, 255.0))
    probs = hist.astype(np.float32)
    total = float(probs.sum())
    if total <= 0.0:
        return 0.0
    probs /= total
    return float(-np.sum(probs * np.log2(probs + 1e-12)))


def _compute_entropy_columns(gray: np.ndarray, window: int, bins: int) -> np.ndarray:
    height, width = gray.shape
    pad = window // 2
    padded = cv2.copyMakeBorder(gray, 0, 0, pad, pad, cv2.BORDER_REFLECT)
    ent = np.zeros(width, dtype=np.float32)
    hist_bins = np.linspace(0, 255, bins + 1, dtype=np.float32)

    for x in range(width):
        roi = padded[:, x : x + window]
        hist, _ = np.histogram(roi, bins=hist_bins)
        probs = hist.astype(np.float32)
        total = float(probs.sum())
        if total <= 0.0:
            ent[x] = 0.0
            continue
        probs /= total
        ent[x] = -np.sum(probs * np.log2(probs + 1e-12))

    return ent


def _normalize(arr: np.ndarray) -> np.ndarray:
    arr = arr.astype(np.float32)
    min_val = float(arr.min())
    max_val = float(arr.max())
    if max_val - min_val < 1e-6:
        return np.zeros_like(arr, dtype=np.float32)
    return (arr - min_val) / (max_val - min_val)


def _analyze_band(
    lab: np.ndarray,
    start: int,
    end: int,
    config: ColorClusterConfig,
    label: str,
) -> BandStats:
    band = lab[:, start:end, :]
    if band.size == 0:
        raise ValueError(f"Empty band for {label}")

    sampling = band[:: config.sample_step, :: config.sample_step, :].reshape(-1, 3)
    if sampling.shape[0] < config.k_clusters:
        sampling = band.reshape(-1, 3)

    criteria = (cv2.TERM_CRITERIA_EPS + cv2.TERM_CRITERIA_MAX_ITER, 30, 1.0)
    compactness, labels_sample, centers = cv2.kmeans(
        np.float32(sampling),
        config.k_clusters,
        None,
        criteria,
        5,
        cv2.KMEANS_PP_CENTERS,
    )

    pixels = band.reshape(-1, 3)
    distances = np.linalg.norm(pixels[:, None, :] - centers[None, :, :], axis=2)
    labels_full = distances.argmin(axis=1)

    L_channel = band[:, :, 0].reshape(-1)
    total_pixels = float(L_channel.size)
    cluster_stats = []

    for cluster_idx in range(centers.shape[0]):
        mask = labels_full == cluster_idx
        if not np.any(mask):
            continue
        L_values = L_channel[mask]
        mean_L = float(L_values.mean())
        std_L = float(L_values.std())
        entropy_L = _entropy_from_values(L_values, config.entropy_bins)
        weight_score = float(0.6 * (std_L ** 2) + 0.4 * entropy_L)
        coverage_ratio = float(mask.sum() / total_pixels)
        cluster_stats.append(
            {
                "idx": cluster_idx,
                "mean_L": mean_L,
                "std_L": std_L,
                "entropy_L": entropy_L,
                "weight_score": weight_score,
                "coverage_ratio": coverage_ratio,
            }
        )

    if not cluster_stats:
        raise RuntimeError(f"No clusters found for {label}")

    background_cluster = min(cluster_stats, key=lambda item: (item["weight_score"], -item["coverage_ratio"]))
    threshold = background_cluster["mean_L"] + config.std_multiplier * background_cluster["std_L"]

    return BandStats(
        label=label,
        mean_L=background_cluster["mean_L"],
        std_L=background_cluster["std_L"],
        entropy_L=background_cluster["entropy_L"],
        weight_score=background_cluster["weight_score"],
        coverage_ratio=background_cluster["coverage_ratio"],
        threshold=threshold,
    )


def _find_margin(scores: np.ndarray, threshold: float, min_width: int, direction: str) -> Optional[MarginRegion]:
    if direction == "left":
        run_end = 0
        while run_end < scores.size and scores[run_end] >= threshold:
            run_end += 1
        if run_end >= min_width:
            segment = scores[:run_end]
            mean_score = float(segment.mean())
            return MarginRegion(0, run_end - 1, mean_score, mean_score)
        return None

    if direction == "right":
        run_start = scores.size - 1
        while run_start >= 0 and scores[run_start] >= threshold:
            run_start -= 1
        run_start += 1
        width = scores.size - run_start
        if width >= min_width:
            segment = scores[run_start:]
            mean_score = float(segment.mean())
            return MarginRegion(run_start, scores.size - 1, mean_score, mean_score)
        return None

    raise ValueError(f"Unsupported direction: {direction}")


def _find_center_band(scores: np.ndarray, threshold: float, max_width: int) -> Optional[MarginRegion]:
    best_region: Optional[MarginRegion] = None
    run_start = None

    for idx, value in enumerate(scores):
        if value >= threshold:
            if run_start is None:
                run_start = idx
        elif run_start is not None:
            run_end = idx - 1
            width = run_end - run_start + 1
            if width <= max_width:
                segment = scores[run_start : run_end + 1]
                mean_score = float(segment.mean())
                candidate = MarginRegion(run_start, run_end, mean_score, mean_score)
                if best_region is None or candidate.confidence > best_region.confidence:
                    best_region = candidate
            run_start = None

    if run_start is not None:
        run_end = scores.size - 1
        width = run_end - run_start + 1
        if width <= max_width:
            segment = scores[run_start : run_end + 1]
            mean_score = float(segment.mean())
            candidate = MarginRegion(run_start, run_end, mean_score, mean_score)
            if best_region is None or candidate.confidence > (best_region.confidence if best_region else -1):
                best_region = candidate

    return best_region


def _compute_scores(
    L_channel: np.ndarray,
    entropy: np.ndarray,
    stats: BandStats,
    slice_start: int,
    slice_end: int,
) -> np.ndarray:
    col_means = L_channel[:, slice_start:slice_end].mean(axis=0)
    col_stds = L_channel[:, slice_start:slice_end].std(axis=0)
    entropy_slice = entropy[slice_start:slice_end]
    entropy_norm = _normalize(entropy_slice)

    diff = np.abs(col_means - stats.threshold)
    gaussian = np.exp(-0.5 * (diff / (stats.std_L + 1e-3)) ** 2)
    std_factor = np.exp(-0.5 * ((col_stds - stats.std_L) / (stats.std_L + 1e-3)) ** 2)
    score = 0.6 * gaussian + 0.2 * std_factor + 0.2 * (1.0 - entropy_norm)
    return np.clip(score, 0.0, 1.0)


def analyze_image(image: np.ndarray, config: ColorClusterConfig) -> tuple[ClusterResult, np.ndarray]:
    height, width = image.shape[:2]
    lab = _lab_image(image)
    L_channel = lab[:, :, 0]

    left_width = max(1, int(width * config.side_band_ratio))
    right_start = max(0, width - left_width)
    center_half = max(1, int(width * config.center_band_ratio / 2))
    center_start = max(0, width // 2 - center_half)
    center_end = min(width, width // 2 + center_half)

    left_stats = _analyze_band(lab, 0, left_width, config, "left")
    right_stats = _analyze_band(lab, right_start, width, config, "right")
    center_stats = _analyze_band(lab, center_start, center_end, config, "center")

    entropy_cols = _compute_entropy_columns(L_channel, config.entropy_window, config.entropy_bins)

    left_scores = _compute_scores(L_channel, entropy_cols, left_stats, 0, left_width)
    right_scores = _compute_scores(L_channel, entropy_cols, right_stats, right_start, width)
    center_scores = _compute_scores(L_channel, entropy_cols, center_stats, center_start, center_end)

    min_margin_width = max(3, int(width * config.min_margin_ratio))
    max_center_width = max(3, int(width * config.max_center_ratio))

    left_region = _find_margin(left_scores, config.background_score_threshold, min_margin_width, "left")
    right_region = _find_margin(right_scores, config.background_score_threshold, min_margin_width, "right")
    if right_region:
        right_region = MarginRegion(
            right_region.start_x + right_start,
            right_region.end_x + right_start,
            right_region.mean_score,
            right_region.confidence,
        )

    center_region = _find_center_band(center_scores, config.background_score_threshold, max_center_width)
    if center_region:
        center_region = MarginRegion(
            center_region.start_x + center_start,
            center_region.end_x + center_start,
            center_region.mean_score,
            center_region.confidence,
        )

    debug_image = _build_debug_image(
        image,
        left_scores,
        right_scores,
        center_scores,
        left_region,
        right_region,
        center_region,
        left_width,
        right_start,
        center_start,
        center_end,
    )

    result = ClusterResult(
        width=width,
        height=height,
        left_margin=left_region,
        right_margin=right_region,
        center_band=center_region,
        left_band_stats=left_stats,
        right_band_stats=right_stats,
        center_band_stats=center_stats,
    )

    return result, debug_image


def _build_debug_image(
    image: np.ndarray,
    left_scores: np.ndarray,
    right_scores: np.ndarray,
    center_scores: np.ndarray,
    left: Optional[MarginRegion],
    right: Optional[MarginRegion],
    center: Optional[MarginRegion],
    left_width: int,
    right_start: int,
    center_start: int,
    center_end: int,
) -> np.ndarray:
    overlay = image.copy()
    height, width = image.shape[:2]

    if left:
        cv2.rectangle(overlay, (left.start_x, 0), (left.end_x, height - 1), (0, 180, 255), 2)
    if right:
        cv2.rectangle(overlay, (right.start_x, 0), (right.end_x, height - 1), (255, 0, 180), 2)
    if center:
        cv2.rectangle(overlay, (center.start_x, 0), (center.end_x, height - 1), (0, 0, 255), 2)
        cv2.line(overlay, ((center.start_x + center.end_x) // 2, 0), ((center.start_x + center.end_x) // 2, height - 1), (0, 0, 255), 2)

    score_plot_height = 120
    plot = np.zeros((score_plot_height, width), dtype=np.uint8)

    def paint_scores(scores: np.ndarray, start: int) -> None:
        normalized = np.clip(scores, 0.0, 1.0)
        columns = np.int32(normalized * 255)
        for idx, value in enumerate(columns):
            x = start + idx
            plot[:, x] = max(plot[:, x].max(), value)

    paint_scores(left_scores, 0)
    paint_scores(right_scores, right_start)
    paint_scores(center_scores, center_start)

    plot_colored = cv2.applyColorMap(plot, cv2.COLORMAP_PLASMA)
    combined = np.vstack([overlay, plot_colored])

    # Draw band demarcations for clarity.
    cv2.line(combined, (left_width, 0), (left_width, height - 1), (0, 255, 255), 1)
    cv2.line(combined, (right_start, 0), (right_start, height - 1), (255, 255, 0), 1)
    cv2.line(combined, (center_start, 0), (center_start, height - 1), (255, 255, 255), 1)
    cv2.line(combined, (center_end - 1, 0), (center_end - 1, height - 1), (255, 255, 255), 1)

    return combined


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Validate color clustering based dark margin detection.")
    parser.add_argument("--input", required=True, type=Path, help="Path to the source image file")
    parser.add_argument("--output-debug", type=Path, help="Optional path to save the debug visualization")
    parser.add_argument("--show", action="store_true", help="Display the visualization window")
    parser.add_argument("--threshold", type=float, default=None, help="Override the acceptance threshold for background scores")
    parser.add_argument("--std-mult", type=float, default=None, help="Override the standard deviation multiplier for local thresholds")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    config = ColorClusterConfig()
    if args.threshold is not None:
        config.background_score_threshold = args.threshold
    if args.std_mult is not None:
        config.std_multiplier = args.std_mult

    image = _load_image(args.input)
    result, debug_image = analyze_image(image, config)
    print(result.to_json())

    if args.output_debug:
        args.output_debug.parent.mkdir(parents=True, exist_ok=True)
        cv2.imwrite(str(args.output_debug), debug_image)

    if args.show:
        cv2.imshow("color-cluster", debug_image)
        cv2.waitKey(0)
        cv2.destroyAllWindows()


if __name__ == "__main__":
    main()
