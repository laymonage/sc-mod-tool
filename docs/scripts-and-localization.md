# Scripts and localization

The files that say which sound a scene plays and what its subtitle says. Their
details matter for [subtitle mapping](subtitle-mapping.md).

## Sound packages: `Sounds/*.uax`

Unreal packages of `Sound` objects, such as `Lambert.uax` and
`S1_1_2Voice.uax`. A sound is named `Package.Sound`, such as
`Lambert.Play_AlarmStage1Opt1`. Each `Sound` export's serialized data is:

1. one zero byte (an empty property list)
2. the 32-bit key of the sound's event in the [sound maps](sound-maps.md)
3. for lip-synced sounds: a 32-bit `1`, then a length-prefixed, null-terminated
   path such as `\S0_0_2Voice\00_15_01.bin`

Parsed in [`SoundData.cs`](../core/uelib-bridge/SoundData.cs). Only the key
is used.

Lip-sync files (`LIPSYNCH/int/<package>/<line>.bin`) only ship for English and
only for some lines. The 32-bit value at `+8` is the line's duration in
milliseconds, which helped confirm matches but isn't used.

## Scripts: `system/*.u`

Scene logic lives in "pattern" classes in `EchelonPattern.u`, such as
`P_1_1_2_Tbilisi_Coronary`, decompiled with UELib through the
[bridge](../core/uelib-bridge/Exports.cs). Sounds are played through three
native functions (the decompiler shows only their numbers):

| Function | Used for | Example |
|---|---|---|
| `__NFUN_1407__` | Radio messages, with a subtitle | `__NFUN_1407__(Localize("P_0_0_2_Training_Comms", "Speech_0012L", "Localization\\P_0_0_2_Training"), Sound'S0_0_2Voice.Play_00_20_01', …)` |
| `__NFUN_1408__` | Conversation lines, without a subtitle | `__NFUN_1408__(Sound'S1_1_2Voice.Play_11_32_24', 0, …)` |
| `__NFUN_264__` | `PlaySound` on an actor: effects, PA announcements, the far end of phone calls | `SoundActors[0].__NFUN_264__(Sound'DestroyableObjet.Play_ExplosionBridge', 0)` |

[`DirectPairs.cs`](../core/uelib-bridge/DirectPairs.cs) finds `Localize` +
`Sound` pairs, and every sound call in script order with the function it's
passed to.

## Localization: `system/LOCALIZATION/*.<extension>`

INI-style files in Windows-1252. The extension is the language: `int`
(English), `fra`, `deu`, `ita`, `esp`. Each section is named after a pattern
class:

```ini
[P_1_1_2_Tbilisi_Coronary]
Speech_0020L=One false move and you're dead. I need information.
Speech_0021L=Um...Okay.
...
```

- `Speech_…` lines are spoken. `Note_…` and `Goal_…` lines are on-screen text
  and are never voiced.
- Cue numbers don't follow the order lines are spoken, or the sound numbers.
  `Play_11_32_24` is `Speech_0020L`.
- A section can repeat a cue name with different text (`Coronary` has two
  `Speech_0028L` lines), so lines must be read in file order, not looked up by
  name alone.
- A line whose text is `(null)` is a placeholder for a line without text. It
  still takes its place in the order and is matched to its audio, but its
  `Subtitle::text` is `None`, so the placeholder never reaches the app as
  dialogue.
- Other languages' sections usually have the same lines in the same order as
  English, but can keep extra leftover lines. German
  `[P_2_1_2_CIA_EndingP2]` has the 37 English lines plus 4 older drafts at the
  end.
- `HUD.<extension>` names each level, which the app uses to group banks by
  mission (see [mission codes](README.md#mission-codes)).
