# Catabolic

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

**One catalog for all your media.**

Your movies might live on one drive, your music on another, and your books on a
NAS. Catabolic brings them into one searchable catalog and creates organized
folders for the apps you use—without moving or renaming your originals.

Use it yourself or let an AI agent help curate the collection. You decide what
each file is and how it should be organized; Catabolic keeps track of the files
and builds the folders.

[Get started](https://github.com/Jagalite/catabolic/blob/main/docs/GETTING_STARTED.md) · [Documentation](https://github.com/Jagalite/catabolic/wiki) · [FAQ](https://github.com/Jagalite/catabolic/blob/main/docs/FAQ.md)

## What can you do with it?

- **Bring your collection together.** Catalog media across several drives and
  mounted storage locations, and search the recorded catalog while they're offline.
- **Organize it your way.** Add titles, identities, tags, and relationships to
  movies, TV, music, books, audiobooks, comics, photos, documents, and more.
- **Give each app its own library.** Create folders for Plex, Jellyfin, music
  servers, readers, or your own filing system from the same source files.
- **Make collections from searches.** Save a selection, such as favorite films or
  books by an author, and refresh it into a folder when you choose.
- **Keep useful metadata alongside your media.** Export catalog details,
  playlists, and metadata files for other tools.

For example, the same film can appear in your Plex library and a favorites folder
without storing another copy of the movie. Catabolic creates links to the existing
file. You can preview the changes before applying them.

## Works with your media apps

Folder presets cover **Plex, Jellyfin, Emby, Kodi, Infuse, Navidrome,
Audiobookshelf, Komga, Kavita**, and other applications. Custom naming rules let
you build a different structure. There are also explicit import options for
calibre, Calibre-Web, and Immich.

See [supported applications](https://github.com/Jagalite/catabolic/blob/main/docs/COMPATIBILITY.md) for the full list and each
integration's requirements. Folder presets and import options behave differently;
imports copy or upload selected media.

## Install

Requires **Python 3.11+** on **macOS or Linux**.

```sh
python3 -m venv ~/.venvs/catabolic
. ~/.venvs/catabolic/bin/activate
python -m pip install 'git+https://github.com/Jagalite/catabolic.git'
catabolic --help
```

Prefer pipx or want to install a specific version? See the
[installation guide](https://github.com/Jagalite/catabolic/blob/main/docs/INSTALLATION.md).

## Try it without touching your library

The [getting started walkthrough](https://github.com/Jagalite/catabolic/blob/main/docs/GETTING_STARTED.md) creates a small sample
collection and walks through cataloging, tagging, and generating your first
folders. No media server, API key, or FFmpeg installation is needed.

Then add your own source locations, identify the files you want to organize,
choose an output format, and preview the result.

Catabolic is a command-line application and is currently **alpha**. Keep backups
and start with a small collection. Generated folders need access to their source
files; they are not independent backups.

## Learn more

- [Wiki](https://github.com/Jagalite/catabolic/wiki) — walkthroughs and detailed guides.
- [Using Catabolic with agents](https://github.com/Jagalite/catabolic/blob/main/docs/AUTOMATION.md) — automation and structured output.
- [SQL queries](https://github.com/Jagalite/catabolic/blob/main/docs/QUERYING.md) and [GraphQL](https://github.com/Jagalite/catabolic/blob/main/docs/GRAPHQL.md) — explore the catalog.
- [Troubleshooting](https://github.com/Jagalite/catabolic/blob/main/docs/TROUBLESHOOTING.md) — common questions and recovery steps.
- [Development](https://github.com/Jagalite/catabolic/blob/main/docs/DEVELOPMENT.md) — contribute, run tests, and maintain the docs.

Documentation is also available offline: run `catabolic docs` after installation.

## License

[MIT](https://github.com/Jagalite/catabolic/blob/main/LICENSE). Copyright 2026 Jaga Tranvo and The Catabolic Contributors.
