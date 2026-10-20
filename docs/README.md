# Splinter Cell's sound files

What SC Mod Tool knows about Splinter Cell 1's (PC) sound files, worked out
from the game's data. Everything was checked against the English and German
release, and by listening where noted.

- [Sound banks](sound-banks.md): the files that hold the audio, the `.LS0` and
  `.SS0` streams.
- [Sound maps](sound-maps.md): `MAPS.SM0` and `MAPS.LM0`, which list every
  sound, what plays it and where its audio is.
- [Scripts and localization](scripts-and-localization.md): the sound packages
  that name each sound, the scripts that play them and the subtitle text.
- [Subtitle mapping](subtitle-mapping.md): how the app puts the three together
  to find each line's subtitle.
- [Editing notes](editing-notes.md): what's known so far about replacing
  sounds, which the app doesn't do yet.

## The `Sounds` folder

| Path | What it holds |
|---|---|
| `<language>/` | One folder per language: `ENGLISH`, `french`, `GERMAN`, `ITALIEN`, `SPANISH` |
| `<language>/<code>.LS0` | A level's voice lines, such as `1_1_0.LS0` ([banks](sound-banks.md#numeric-banks)) |
| `<language>/Music_*.LS0` | Lambert's alarm warnings and guards' alerts, despite the name ([banks](sound-banks.md#music_ls0)) |
| `<language>/MAPS.LM0` | The localized sound map ([maps](sound-maps.md)) |
| `MAPS.SM0` | The sound map every language shares ([maps](sound-maps.md)) |
| `Music_*.SS0`, `STREAM.SS0`, `1_3_3.SS0` | Music and long streams ([banks](sound-banks.md#stream-files-ss0)) |
| `Common/` | Music for the extra maps ([banks](sound-banks.md#stream-files-ss0)) |
| `<map>/` | An extra map's own sounds ([below](#extra-maps)) |
| `Echelon.SP0` | The sound project, which vgmstream's notes say lists the bank or map files a game uses. The app doesn't read it. |
| `*.uax` | Sound packages, which name each sound ([scripts](scripts-and-localization.md#sound-packages-soundsuax)) |

### Extra maps

Three extra levels keep their own sounds in folders beside the languages:
`1_6_1_1kolacell` (Kola Cell), `1_7_1_1VselkaInfiltration` (Vselka
Infiltration) and `1_7_1_2Vselka` (Vselka Submarine). Each has its own
`MAPS.SM0`, `STREAM.SS0` and `Echelon.SP0`, and a folder per language it ships
(`English`, `French`, `Italien`; no German or Spanish) with its own `.LS0`
banks, `Music_Common.LS0`, `Music_Georgie.LS0` and `MAPS.LM0`.

Their `MAPS.SM0` refer to `Music_Common.SS0` and `Music_Georgie.SS0`, which
aren't beside them but in `Sounds/Common`, so the app looks there for a stream
file it can't find. The app lists an extra map's banks under the language,
grouped by map.

## Mission codes

File, folder, script and area names start with a mission code: mission, part
and level, such as `1_1_0` for the Police Station's first level. The game's
`HUD` localization names each level (`1_1_0Tbilisi=Police Station`), which the
app uses to group banks. A two-part code such as `1_1` is a mission's shared
lines, which several of its levels play.

Codes starting with `3_` are a Russian mission that isn't in this release: no
level ships, but `HUD` still names `3_2` "Nuclear Power Plant" and `3_4`
"Severonickel", and its sound packages (`S3_*Voice.uax`) and scenes
(`P_3_4_*Severonickel`) remain. The extra maps, set on the Kola Peninsula, have
the only copies of its voice lines, and only those their own scenes use.
