using System.Text;

namespace SCModUELibBridge;

/// <summary>Finds direct subtitle and sound pairs in decompiled UnrealScript.</summary>
internal static class DirectPairs
{
    private const string LocalizeName = "Localize";
    private const string SoundName = "Sound";

    /// <summary>
    /// Finds <c>Localize(section, cue, file), Sound'Package.Event'</c> argument
    /// pairs, where a localized cue is passed straight alongside the sound it voices.
    /// </summary>
    internal static IEnumerable<DirectPair> Find(string script)
    {
        var position = 0;
        while ((position = script.IndexOf(LocalizeName, position, StringComparison.Ordinal)) >= 0)
        {
            if (TryReadPair(script, position, out var pair, out _))
            {
                yield return pair;
            }
            position += LocalizeName.Length;
        }
    }

    /// <summary>
    /// Finds every <c>Sound'Package.Event'</c> reference in script order. A sound
    /// passed straight after a <c>Localize(...)</c> call carries that cue.
    /// </summary>
    internal static IEnumerable<SoundCall> FindSoundCalls(string script)
    {
        var position = 0;
        while (position < script.Length)
        {
            var localize = script.IndexOf(LocalizeName, position, StringComparison.Ordinal);
            var sound = script.IndexOf(SoundName + "'", position, StringComparison.Ordinal);
            if (localize < 0 && sound < 0)
            {
                yield break;
            }

            if (localize >= 0 && (sound < 0 || localize < sound))
            {
                if (TryReadPair(script, localize, out var pair, out var end))
                {
                    yield return new SoundCall(pair.SoundPackage, pair.SoundEvent, pair, CallingFunction(script, localize));
                    position = end;
                }
                else
                {
                    position = localize + LocalizeName.Length;
                }
                continue;
            }

            var cursor = sound;
            if ((sound == 0 || !IsIdentifier(script[sound - 1]))
                && ReadIdentifier(script, ref cursor, SoundName)
                && ReadQuotedValue(script, ref cursor, '\'', out var reference)
                && TrySplitSoundReference(reference, out var soundPackage, out var soundEvent))
            {
                yield return new SoundCall(soundPackage, soundEvent, null, CallingFunction(script, sound));
                position = cursor;
            }
            else
            {
                position = sound + SoundName.Length;
            }
        }
    }

    /// <summary>
    /// Returns the name of the function whose first argument starts at
    /// <paramref name="argument"/>, such as <c>__NFUN_1408__</c>.
    /// </summary>
    private static string? CallingFunction(string script, int argument)
    {
        var cursor = argument - 1;
        while (cursor >= 0 && char.IsWhiteSpace(script[cursor]))
        {
            cursor--;
        }
        if (cursor < 0 || script[cursor] != '(')
        {
            return null;
        }

        var end = cursor;
        while (cursor > 0 && IsIdentifier(script[cursor - 1]))
        {
            cursor--;
        }
        return cursor < end ? script[cursor..end] : null;
    }

    /// <summary>Reads a direct pair starting at a <c>Localize</c> name.</summary>
    private static bool TryReadPair(string script, int start, out DirectPair pair, out int end)
    {
        pair = null!;
        end = start;
        var cursor = start + LocalizeName.Length;
        if ((start > 0 && IsIdentifier(script[start - 1]))
            || (cursor < script.Length && IsIdentifier(script[cursor])))
        {
            return false;
        }

        SkipWhitespace(script, ref cursor);
        if (!ReadCharacter(script, ref cursor, '(')
            || !ReadStringArgument(script, ref cursor, out var section)
            || !ReadCharacter(script, ref cursor, ',')
            || !ReadStringArgument(script, ref cursor, out var cue)
            || !ReadCharacter(script, ref cursor, ',')
            || !ReadStringArgument(script, ref cursor, out var localizationPath)
            || !ReadCharacter(script, ref cursor, ')'))
        {
            return false;
        }

        SkipWhitespace(script, ref cursor);
        if (!ReadCharacter(script, ref cursor, ','))
        {
            return false;
        }

        SkipWhitespace(script, ref cursor);
        if (!ReadIdentifier(script, ref cursor, SoundName)
            || !ReadQuotedValue(script, ref cursor, '\'', out var soundReference)
            || !TrySplitSoundReference(soundReference, out var soundPackage, out var soundEvent))
        {
            return false;
        }

        pair = new DirectPair(section, cue, localizationPath, soundPackage, soundEvent);
        end = cursor;
        return true;
    }

    private static bool TrySplitSoundReference(string reference, out string soundPackage, out string soundEvent)
    {
        var separator = reference.LastIndexOf('.');
        if (separator <= 0 || separator == reference.Length - 1)
        {
            soundPackage = string.Empty;
            soundEvent = string.Empty;
            return false;
        }

        soundPackage = reference[..separator];
        soundEvent = reference[(separator + 1)..];
        return true;
    }

    private static bool ReadStringArgument(string text, ref int position, out string value)
    {
        SkipWhitespace(text, ref position);
        if (position >= text.Length || (text[position] != '"' && text[position] != '\''))
        {
            value = string.Empty;
            return false;
        }

        return ReadQuotedValue(text, ref position, text[position], out value);
    }

    private static bool ReadQuotedValue(string text, ref int position, char quote, out string value)
    {
        value = string.Empty;
        if (position >= text.Length || text[position] != quote)
        {
            return false;
        }

        position++;
        var result = new StringBuilder();
        while (position < text.Length)
        {
            var character = text[position++];
            if (character == quote)
            {
                value = result.ToString();
                return true;
            }

            if (character == '\\' && position < text.Length)
            {
                character = text[position++];
                if (character is not '\\' && character != quote)
                {
                    result.Append('\\');
                }
            }

            result.Append(character);
        }

        return false;
    }

    private static bool ReadCharacter(string text, ref int position, char expected)
    {
        SkipWhitespace(text, ref position);
        if (position >= text.Length || text[position] != expected)
        {
            return false;
        }

        position++;
        return true;
    }

    private static bool ReadIdentifier(string text, ref int position, string expected)
    {
        if (!text.AsSpan(position).StartsWith(expected, StringComparison.Ordinal))
        {
            return false;
        }

        position += expected.Length;
        return position >= text.Length || !IsIdentifier(text[position]);
    }

    private static void SkipWhitespace(string text, ref int position)
    {
        while (position < text.Length && char.IsWhiteSpace(text[position]))
        {
            position++;
        }
    }

    private static bool IsIdentifier(char character)
    {
        return char.IsLetterOrDigit(character) || character == '_';
    }
}

internal sealed record DirectPair(
    string Section,
    string Cue,
    string LocalizationPath,
    string SoundPackage,
    string SoundEvent);

/// <summary>
/// A sound reference, with its cue when it's part of a direct pair, and the
/// function it's the first argument of, if any.
/// </summary>
internal sealed record SoundCall(string SoundPackage, string SoundEvent, DirectPair? Pair, string? Function);
