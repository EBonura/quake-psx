# Quake console loading fix, 2026-09-08

Quake on demo disc v0.36 stopped at its own LOADING / ENTRANCE screen on
an original PlayStation, including when it was the first title launched
after power-on. Cortex Ignition, Half-Life, VoXide, NitroXide and the Celeste
collection launched successfully. The same Quake build worked on
SuperStation One and in PSoXide.

## Cause

The forward map reader cached the Nodes lump and paused the CD while the
resident loader expanded each node. On the next cached read, it reopened
ReadN before checking the cache. The remaining cached reads left the drive
running without consuming it, allowing later sectors to be lost while the
CPU converted the nodes.

The reader now opens a stream only to fill the cache or fetch uncached data.
It stays paused throughout cached node conversion and restarts at the next
uncached lump. The extracted reader module lets the host regression execute
the actual guest code with a simulated drive.

Loading also identifies GLOBAL SOUND and MAP DATA. Returned sound and level
load failures display the CD diagnostic word. A zero diagnostic means no
CD-command failure was recorded; it does not validate the file contents.

## Validation

- The simulated-drive regression fails with the original stream ordering and
  passes with the fix. The empty-read case also passes.
- The PS1 release build succeeds. Post-link patching resolves 18 branch-delay
  hazards using 72 of 96 trampoline words; the final scan finds none remaining.
- Both the original release and the candidate reach the menu in PSoXide.
  The candidate also enters New Game, verified from display captures.
- The candidate HL disc differs from v0.36 only within Quake's embedded image
  and its table-of-contents record. Other programs and audio are byte-identical.
- The owner burned and tested the candidate on the original PlayStation and
  confirmed that it works on 2026-09-08. This confirms the reported loading
  failure is resolved; it is not a claim of a complete Episode 1 playthrough.

Console-tested candidate BIN SHA-256:
`900fb7d78f12783c86554fc44e51a82eb447a5218c6477b3089d62119f4c0893`

The candidate used SDK revision
`8df242b353b8a3664c1d2ed20622d692d1349306` and featureless release settings.
