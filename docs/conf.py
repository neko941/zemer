import tomllib
from pathlib import Path

project = "Zemer"
copyright = "2026, Zemer contributors"
author = "Zemer contributors"
workspace_manifest = tomllib.loads((Path(__file__).parent.parent / "Cargo.toml").read_text())
release = workspace_manifest["workspace"]["package"]["version"]

extensions = []
templates_path = ["_templates"]
exclude_patterns = ["_build", "Thumbs.db", ".DS_Store"]

root_doc = "index"
language = "en"
html_theme = "sphinx_rtd_theme"
html_title = "Zemer Audio Library"
