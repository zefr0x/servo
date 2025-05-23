import argparse
import bz2
import gzip
import json
import io
import os
from datetime import datetime, timedelta
from typing import Any, Callable, List, Optional, Text, cast
from urllib.request import urlopen

try:
    import zstandard
except ImportError:
    zstandard = cast(Any, None)

from .utils import git

from . import log


here = os.path.dirname(__file__)

wpt_root = os.path.abspath(os.path.join(here, os.pardir, os.pardir))
logger = log.get_logger()


def abs_path(path: Text) -> Text:
    return os.path.abspath(os.path.expanduser(path))


def should_download(manifest_path: Text, rebuild_time: timedelta = timedelta(days=5)) -> bool:
    if not os.path.exists(manifest_path):
        return True
    mtime = datetime.fromtimestamp(os.path.getmtime(manifest_path))
    if mtime < datetime.now() - rebuild_time:
        return True
    logger.info("Skipping manifest download because existing file is recent")
    return False


def merge_pr_tags(repo_root: Text, max_count: int = 50) -> List[Text]:
    gitfunc = git(repo_root)
    tags: List[Text] = []
    if gitfunc is None:
        return tags
    for line in gitfunc("log", "--format=%D", "--max-count=%s" % max_count).split("\n"):
        for ref in line.split(", "):
            if ref.startswith("tag: merge_pr_"):
                tags.append(ref[5:])
    return tags


def score_name(name: Text) -> Optional[int]:
    """Score how much we like each filename, lower wins, None rejects"""

    # Accept both ways of naming the manifest asset, even though
    # there's no longer a reason to include the commit sha.
    if name.startswith("MANIFEST-") or name.startswith("MANIFEST."):
        if zstandard and name.endswith("json.zst"):
            return 1
        if name.endswith(".json.bz2"):
            return 2
        if name.endswith(".json.gz"):
            return 3
    return None


def github_url(tags: List[Text]) -> Optional[List[Text]]:
    for tag in tags:
        url = "https://api.github.com/repos/web-platform-tests/wpt/releases/tags/%s" % tag
        try:
            resp = urlopen(url)
        except Exception:
            logger.warning("Fetching %s failed" % url)
            continue

        if resp.code != 200:
            logger.warning("Fetching %s failed; got HTTP status %d" % (url, resp.code))
            continue

        try:
            release = json.load(resp.fp)
        except ValueError:
            logger.warning("Response was not valid JSON")
            return None

        candidates = []
        for item in release["assets"]:
            score = score_name(item["name"])
            if score is not None:
                candidates.append((score, item["browser_download_url"]))

        return [item[1] for item in sorted(candidates)]

    return None


def download_manifest(
        manifest_path: Text,
        tags_func: Callable[[], List[Text]],
        url_func: Callable[[List[Text]], Optional[List[Text]]],
        force: bool = False
) -> bool:
    if not force and not should_download(manifest_path):
        return False

    tags = tags_func()

    urls = url_func(tags)
    if not urls:
        logger.warning("No generated manifest found")
        return False

    for url in urls:
        logger.info("Downloading manifest from %s" % url)
        try:
            resp = urlopen(url)
        except Exception:
            logger.warning("Downloading pregenerated manifest failed")
            continue

        if resp.code != 200:
            logger.warning("Downloading pregenerated manifest failed; got HTTP status %d" %
                           resp.code)
            continue

        if url.endswith(".zst"):
            if not zstandard:
                continue
            try:
                dctx = zstandard.ZstdDecompressor()
                decompressed = dctx.decompress(resp.read())
            except OSError:
                logger.warning("Failed to decompress downloaded file")
                continue
        elif url.endswith(".bz2"):
            try:
                decompressed = bz2.decompress(resp.read())
            except OSError:
                logger.warning("Failed to decompress downloaded file")
                continue
        elif url.endswith(".gz"):
            fileobj = io.BytesIO(resp.read())
            try:
                with gzip.GzipFile(fileobj=fileobj) as gzf:
                    data = gzf.read()
                    decompressed = data
            except OSError:
                logger.warning("Failed to decompress downloaded file")
                continue
        else:
            logger.warning("Unknown file extension: %s" % url)
            continue
        break
    else:
        return False

    try:
        with open(manifest_path, "wb") as f:
            f.write(decompressed)
    except Exception:
        logger.warning("Failed to write manifest")
        return False
    logger.info("Manifest downloaded")
    return True


def create_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "-p", "--path", type=abs_path, help="Path to manifest file.")
    parser.add_argument(
        "--tests-root", type=abs_path, default=wpt_root, help="Path to root of tests.")
    parser.add_argument(
        "--force", action="store_true",
        help="Always download, even if the existing manifest is recent")
    return parser


def download_from_github(path: Text, tests_root: Text, force: bool = False) -> bool:
    return download_manifest(path, lambda: merge_pr_tags(tests_root), github_url,
                             force=force)


def run(**kwargs: Any) -> int:
    if kwargs["path"] is None:
        path = os.path.join(kwargs["tests_root"], "MANIFEST.json")
    else:
        path = kwargs["path"]
    success = download_from_github(path, kwargs["tests_root"], kwargs["force"])
    return 0 if success else 1
