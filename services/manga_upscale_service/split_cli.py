"""CLI entrypoint for the Phase 1 double page split prototype."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

import cv2
import numpy as np

if __package__ in (None, ""):
    import importlib.util

    current_dir = Path(__file__).resolve().parent
    module_path = current_dir / "doublepage_split.py"
    spec = importlib.util.spec_from_file_location(
        "manga_upscale_service.doublepage_split",
        module_path,
    )
    module = importlib.util.module_from_spec(spec)  # type: ignore[arg-type]
    sys.modules.setdefault(spec.name, module)
    assert spec.loader is not None
    spec.loader.exec_module(module)  # type: ignore[arg-type]
else:
    from . import doublepage_split as module

SplitConfig = module.SplitConfig
SplitResult = module.SplitResult
iter_supported_images = module.iter_supported_images
split_image = module.split_image


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Content-aware double page splitter prototype.",
    )
    parser.add_argument("input", type=Path, help="Image file or directory to process.")
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("split-output"),
        help="Directory to write processed images and reports.",
    )
    parser.add_argument(
        "--padding-ratio",
        type=float,
        default=SplitConfig.padding_ratio,
        help="Extra padding applied when cropping (fraction of dimension).",
    )
    parser.add_argument(
        "--cover-threshold",
        type=float,
        default=SplitConfig.cover_content_ratio,
        help="Maximum content width ratio to classify as cover.",
    )
    parser.add_argument(
        "--confidence-threshold",
        type=float,
        default=SplitConfig.confidence_threshold,
        help="Minimum valley contrast required to accept the smart split.",
    )
    parser.add_argument(
        "--edge-exclusion",
        type=float,
        default=SplitConfig.edge_exclusion_ratio,
        help="Fraction of width to ignore near edges when searching for valleys.",
    )
    parser.add_argument(
        "--min-foreground",
        type=float,
        default=SplitConfig.min_foreground_ratio,
        help="Skip images with less foreground than this ratio.",
    )
    parser.add_argument(
        "--overwrite",
        action="store_true",
        help="Overwrite output files if they already exist.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Run analysis without writing image outputs.",
    )
    parser.add_argument(
        "--report",
        type=Path,
        default=None,
        help="Optional path for the JSON report (defaults to output/split-report.json).",
    )
    return parser


def _make_config(args: argparse.Namespace) -> SplitConfig:
    return SplitConfig(
        padding_ratio=args.padding_ratio,
        cover_content_ratio=args.cover_threshold,
        confidence_threshold=args.confidence_threshold,
        edge_exclusion_ratio=args.edge_exclusion,
        min_foreground_ratio=args.min_foreground,
    )


def _write_page(output_dir: Path, target: Path, image, *, overwrite: bool) -> None:
    if target.exists() and not overwrite:
        raise FileExistsError(f"Output file already exists: {target}")
    output_dir.mkdir(parents=True, exist_ok=True)
    if not cv2.imwrite(str(target), image):
        raise RuntimeError(f"Failed to write image: {target}")


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    input_path = args.input.expanduser().resolve()
    output_dir = args.output.expanduser().resolve()
    report_path = args.report.expanduser().resolve() if args.report else output_dir / "split-report.json"

    if not input_path.exists():
        parser.error(f"Input path does not exist: {input_path}")

    config = _make_config(args)

    results: list[dict[str, Any]] = []
    processed = 0

    for source in iter_supported_images(input_path):
        image = cv2.imread(str(source))
        if image is None:
            print(f"[warn] Skipping unreadable image: {source}", file=sys.stderr)
            continue

        result = split_image(image, config=config)
        outputs: list[str] = []

        if not args.dry_run:
            outputs = _export_result(result, source, output_dir, args.overwrite)

        entry = {
            "source": str(source.resolve()),
            "mode": result.mode,
            "split_x": result.split_x,
            "confidence": result.confidence,
            "content_width_ratio": result.content_width_ratio,
            "outputs": outputs,
            "metadata": _sanitize_metadata(result.metadata),
        }
        results.append(entry)
        processed += 1

    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps({"items": results}, indent=2), encoding="utf-8")

    print(f"Processed {processed} file(s). Report: {report_path}")
    return 0


def _sanitize_metadata(metadata: dict[str, Any]) -> dict[str, Any]:
    def _to_serialisable(value: Any) -> Any:
        if isinstance(value, (int, float, str, type(None))):
            return value
        if isinstance(value, np.generic):
            return value.item()
        if isinstance(value, dict):
            return {str(k): _to_serialisable(v) for k, v in value.items()}
        if isinstance(value, (list, tuple)):
            return [_to_serialisable(v) for v in value]
        if isinstance(value, np.ndarray):
            return value.tolist()
        return str(value)

    return {str(key): _to_serialisable(val) for key, val in metadata.items()}


def _export_result(
    result: SplitResult,
    source: Path,
    output_dir: Path,
    overwrite: bool,
) -> list[str]:
    suffix = source.suffix or ".png"
    outputs: list[str] = []

    if result.mode == "skip":
        return outputs

    if result.mode == "cover-trim":
        filename = f"{source.stem}_cover{suffix}"
        target = output_dir / filename
        _write_page(output_dir, target, result.pages[0], overwrite=overwrite)
        outputs.append(str(filename))
        return outputs

    if not result.pages:
        return outputs

    names = [f"{source.stem}_R{suffix}", f"{source.stem}_L{suffix}"]
    for page, name in zip(result.pages, names):
        target = output_dir / name
        _write_page(output_dir, target, page, overwrite=overwrite)
        outputs.append(str(name))

    return outputs


if __name__ == "__main__":
    raise SystemExit(main())

