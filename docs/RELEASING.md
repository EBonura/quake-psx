# Releasing quake-psx

`.github/workflows/itch-release.yml` builds the standalone PlayStation disc and
can upload it to the private `bonnie-studios/quake-psx` itch.io project. It
does not build or publish the PSoXide demo disc, Half-Life or another game.

## Repository setup

Add an itch.io API key to the GitHub repository as an Actions secret named
`BUTLER_API_KEY`. Keep the itch.io project private until the release is ready.

The workflow has read-only GitHub permissions. The key is used only by the
Butler upload steps.

## Tagged release

Release tags use a version such as `v0.1.0` or `v0.1.0-rc.1` and must point to
a commit on `main`:

```sh
git switch main
git pull --ff-only
git tag -a v0.1.0 -m "quake-psx 0.1.0"
git push origin v0.1.0
```

The workflow then:

1. checks the tag and branch;
2. checks out the pinned PSoXide revision;
3. builds the standalone disc;
4. verifies its provenance file and artifact names;
5. stores the packaged release as a GitHub Actions artifact;
6. uploads the same files to itch.io with Butler.

The release folder contains the BIN/CUE pair, PS-X EXE, provenance JSON,
version, player README, licence and third-party notices. Generated game data
and disc images are never committed to Git.

## Test without uploading

Run **Build and publish quake-psx** manually in GitHub Actions, enter a version
and leave **Upload the verified build to the private itch.io project** disabled.
This runs the complete packaging job and retains the artifact without calling
Butler.

Enabling the upload option publishes a real build. The workflow does not change
the itch.io page's private/public setting.
