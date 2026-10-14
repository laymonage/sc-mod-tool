using System.Text;

namespace SCModUELibBridge;

/// <summary>Finds direct subtitle and sound pairs in decompiled UnrealScript.</summary>
internal static class DirectPairs
{
    /// <summary>
    /// Finds <c>Localize(section, cue, file), Sound'Package.Event'</c> argument
    /// pairs, where a localized cue is passed straight alongside the sound it voices.
    /// </summary>
    internal static IEnumerable<DirectPair> Find(string script)
    {
        const string localizeName = "Localize";
        var position = 0;
        while ((position = script.IndexOf(localizeName, position, StringComparison.Ordinal)) >= 0)
        {
            var start = position;
            position += localizeName.Length;
            if ((start > 0 && IsIdentifier(script[start - 1]))
                || (position < script.Length && IsIdentifier(script[position])))
            {
                continue;
            }

            var cursor = position;
            SkipWhitespace(script, ref cursor);
            if (!ReadCharacter(script, ref cursor, '(')
                || !ReadStringArgument(script, ref cursor, out var section)
                || !ReadCharacter(script, ref cursor, ',')
                || !ReadStringArgument(script, ref cursor, out var cue)
                || !ReadCharacter(script, ref cursor, ',')
                || !ReadStringArgument(script, ref cursor, out var localizationPath)
                || !ReadCharacter(script, ref cursor, ')'))
            {
                continue;
            }

            SkipWhitespace(script, ref cursor);
            if (!ReadCharacter(script, ref cursor, ','))
            {
                continue;
            }

            SkipWhitespace(script, ref cursor);
            if (!ReadIdentifier(script, ref cursor, "Sound")
                || !ReadQuotedValue(script, ref cursor, '\'', out var soundReference)
                || !TrySplitSoundReference(soundReference, out var soundPackage, out var soundEvent))
            {
                continue;
            }

            yield return new DirectPair(section, cue, localizationPath, soundPackage, soundEvent);
        }
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
