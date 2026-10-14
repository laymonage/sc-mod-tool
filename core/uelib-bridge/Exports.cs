using System.Buffers;
using System.Buffers.Binary;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using UELib;
using UELib.Core;

namespace ScModUelibBridge;

public static unsafe class Exports
{
    [UnmanagedCallersOnly(EntryPoint = "scmod_scan_script_audio_links", CallConvs = [typeof(CallConvCdecl)])]
    public static int ScanScriptAudioLinks(byte* gameRootUtf8, byte* outputUtf8, int outputCapacity)
    {
        try
        {
            var gameRoot = Marshal.PtrToStringUTF8((nint)gameRootUtf8);
            if (gameRoot is null)
            {
                return WriteFailure(outputUtf8, outputCapacity, "Game directory path is null.");
            }

            var result = AnalyzeGame(gameRoot);
            return WriteResult(outputUtf8, outputCapacity, result);
        }
        catch (Exception error)
        {
            return WriteFailure(outputUtf8, outputCapacity, error.ToString());
        }
    }

    private static BridgeResult AnalyzeGame(string gameRoot)
    {
        var systemRoot = Path.Combine(gameRoot, "system");
        var soundsRoot = Path.Combine(gameRoot, "Sounds");
        if (!Directory.Exists(systemRoot))
        {
            throw new DirectoryNotFoundException($"Game system directory was not found: {systemRoot}");
        }
        if (!Directory.Exists(soundsRoot))
        {
            throw new DirectoryNotFoundException($"Game Sounds directory was not found: {soundsRoot}");
        }

        var issues = new List<string>();
        var sourcePairs = new HashSet<SourcePair>();
        foreach (var packagePath in FindScriptPackages(systemRoot))
        {
            try
            {
                using var package = UnrealLoader.LoadPackage(packagePath, FileAccess.Read);
                package.InitializePackage(
                    UnrealPackage.InitFlags.Construct | UnrealPackage.InitFlags.RegisterClasses);

                foreach (var scriptObject in package.Objects
                             .OfType<UStruct>()
                             .Where(item => item is UFunction || (item is UState && item is not UClass)))
                {
                    var sourceObject = scriptObject.GetReferencePath();
                    try
                    {
                        scriptObject.BeginDeserializing();
                        var script = scriptObject.Decompile();
                        foreach (var pair in FindDirectPairs(script))
                        {
                            sourcePairs.Add(new SourcePair(
                                Path.GetFileName(packagePath),
                                sourceObject,
                                pair.Section,
                                pair.Cue,
                                LocalizationBaseName(pair.LocalizationPath),
                                pair.SoundPackage,
                                pair.SoundEvent));
                        }
                    }
                    catch (Exception error)
                    {
                        issues.Add($"Could not decompile {sourceObject} from {packagePath}: {error.Message}");
                    }
                }
            }
            catch (Exception error)
            {
                issues.Add($"Could not load script package {packagePath}: {error.Message}");
            }
        }

        var soundPackages = new Dictionary<string, SoundPackage>(StringComparer.OrdinalIgnoreCase);
        var links = new List<BridgeLink>();
        try
        {
            foreach (var pair in sourcePairs)
            {
                if (!soundPackages.TryGetValue(pair.SoundPackage, out var soundPackage))
                {
                    var path = FindPackagePath(soundsRoot, pair.SoundPackage + ".uax");
                    if (path is null)
                    {
                        issues.Add($"Sound package was not found for {pair.SoundPackage}.{pair.SoundEvent}.");
                        continue;
                    }

                    try
                    {
                        var package = UnrealLoader.LoadPackage(path, FileAccess.Read);
                        package.InitializePackage(
                            UnrealPackage.InitFlags.Construct | UnrealPackage.InitFlags.RegisterClasses);
                        soundPackage = new SoundPackage(package, File.ReadAllBytes(path), path);
                        soundPackages.Add(pair.SoundPackage, soundPackage);
                    }
                    catch (Exception error)
                    {
                        issues.Add($"Could not load sound package {path}: {error.Message}");
                        continue;
                    }
                }

                if (!TryReadSoundMapKey(soundPackage, pair.SoundEvent, out var key, out var issue))
                {
                    issues.Add(issue);
                    continue;
                }

                links.Add(new BridgeLink(
                    pair.SourcePackage,
                    pair.SourceObject,
                    pair.Section,
                    pair.Cue,
                    pair.LocalizationFile,
                    pair.SoundPackage + "." + pair.SoundEvent,
                    key));
            }
        }
        finally
        {
            foreach (var soundPackage in soundPackages.Values)
            {
                soundPackage.Package.Dispose();
            }
        }

        links.Sort((left, right) =>
        {
            var bySection = string.Compare(left.Section, right.Section, StringComparison.OrdinalIgnoreCase);
            if (bySection != 0)
            {
                return bySection;
            }

            var byCue = string.Compare(left.Cue, right.Cue, StringComparison.OrdinalIgnoreCase);
            return byCue != 0
                ? byCue
                : string.Compare(left.SoundEvent, right.SoundEvent, StringComparison.OrdinalIgnoreCase);
        });
        return new BridgeResult(links, issues);
    }

    private static IEnumerable<string> FindScriptPackages(string systemRoot)
    {
        var packages = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (var path in Directory.EnumerateFiles(systemRoot, "*.u", SearchOption.TopDirectoryOnly))
        {
            packages[Path.GetFileName(path)] = path;
        }

        var updateRoot = Path.Combine(systemRoot, "update");
        if (Directory.Exists(updateRoot))
        {
            foreach (var path in Directory.EnumerateFiles(updateRoot, "*.u", SearchOption.TopDirectoryOnly))
            {
                packages[Path.GetFileName(path)] = path;
            }
        }

        return packages.Values.Order(StringComparer.OrdinalIgnoreCase).ToArray();
    }

    private static string? FindPackagePath(string directory, string fileName)
    {
        var exactPath = Path.Combine(directory, fileName);
        if (File.Exists(exactPath))
        {
            return exactPath;
        }

        return Directory.EnumerateFiles(directory, "*.uax", SearchOption.TopDirectoryOnly)
            .FirstOrDefault(path => Path.GetFileName(path).Equals(fileName, StringComparison.OrdinalIgnoreCase));
    }

    private static string LocalizationBaseName(string localizationPath)
    {
        var separator = Math.Max(localizationPath.LastIndexOf('/'), localizationPath.LastIndexOf('\\'));
        var baseName = localizationPath[(separator + 1)..];
        return Path.GetFileNameWithoutExtension(baseName);
    }

    private static bool TryReadSoundMapKey(
        SoundPackage package,
        string soundEvent,
        out uint key,
        out string issue)
    {
        key = 0;
        var sound = package.Package.FindObjectByGroup(package.PackageName + "." + soundEvent);
        if (sound is null || sound.ExportTable is null)
        {
            issue = $"Sound object {package.PackageName}.{soundEvent} was not found in {package.Path}.";
            return false;
        }

        if (sound.Class is null || sound.Class.Name != "Sound")
        {
            issue = $"Object {package.PackageName}.{soundEvent} is not an Engine.Sound export.";
            return false;
        }

        var serialOffset = sound.ExportTable.SerialOffset;
        var serialSize = sound.ExportTable.SerialSize;
        if (serialOffset < 0 || serialSize < 5 || serialOffset > package.Bytes.Length - 5)
        {
            issue = $"Sound object {package.PackageName}.{soundEvent} has an invalid serialized range.";
            return false;
        }

        if (package.Bytes[serialOffset] != 0)
        {
            issue = $"Sound object {package.PackageName}.{soundEvent} uses an unsupported serialized key layout.";
            return false;
        }

        key = BinaryPrimitives.ReadUInt32LittleEndian(package.Bytes.AsSpan(serialOffset + 1, sizeof(uint)));
        issue = string.Empty;
        return true;
    }

    private static IEnumerable<DirectPair> FindDirectPairs(string script)
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

    private static int WriteResult(byte* outputUtf8, int outputCapacity, BridgeResult result)
    {
        var buffer = new ArrayBufferWriter<byte>();
        using (var writer = new Utf8JsonWriter(buffer))
        {
            writer.WriteStartObject();
            writer.WritePropertyName("links");
            writer.WriteStartArray();
            foreach (var link in result.Links)
            {
                writer.WriteStartObject();
                writer.WriteString("sourcePackage", link.SourcePackage);
                writer.WriteString("sourceObject", link.SourceObject);
                writer.WriteString("section", link.Section);
                writer.WriteString("cue", link.Cue);
                writer.WriteString("localizationFile", link.LocalizationFile);
                writer.WriteString("soundEvent", link.SoundEvent);
                writer.WriteNumber("soundMapKey", link.SoundMapKey);
                writer.WriteEndObject();
            }
            writer.WriteEndArray();
            writer.WritePropertyName("issues");
            writer.WriteStartArray();
            foreach (var issue in result.Issues)
            {
                writer.WriteStringValue(issue);
            }
            writer.WriteEndArray();
            writer.WriteEndObject();
            writer.Flush();
        }

        return CopyOutput(outputUtf8, outputCapacity, buffer.WrittenSpan, 0);
    }

    private static int WriteFailure(byte* outputUtf8, int outputCapacity, string message)
    {
        var buffer = new ArrayBufferWriter<byte>();
        using (var writer = new Utf8JsonWriter(buffer))
        {
            writer.WriteStartObject();
            writer.WriteString("error", message);
            writer.WriteEndObject();
            writer.Flush();
        }

        return CopyOutput(outputUtf8, outputCapacity, buffer.WrittenSpan, -1);
    }

    private static int CopyOutput(byte* outputUtf8, int outputCapacity, ReadOnlySpan<byte> bytes, int status)
    {
        if (outputUtf8 is null || outputCapacity <= bytes.Length)
        {
            return -2;
        }

        bytes.CopyTo(new Span<byte>(outputUtf8, bytes.Length));
        outputUtf8[bytes.Length] = 0;
        return status;
    }

    private sealed record DirectPair(
        string Section,
        string Cue,
        string LocalizationPath,
        string SoundPackage,
        string SoundEvent);

    private sealed record SourcePair(
        string SourcePackage,
        string SourceObject,
        string Section,
        string Cue,
        string LocalizationFile,
        string SoundPackage,
        string SoundEvent);

    private sealed record BridgeLink(
        string SourcePackage,
        string SourceObject,
        string Section,
        string Cue,
        string LocalizationFile,
        string SoundEvent,
        uint SoundMapKey);

    private sealed record BridgeResult(IReadOnlyList<BridgeLink> Links, IReadOnlyList<string> Issues);

    private sealed record SoundPackage(UnrealPackage Package, byte[] Bytes, string Path)
    {
        public string PackageName => System.IO.Path.GetFileNameWithoutExtension(Path);
    }
}
