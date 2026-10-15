using System.Buffers;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using UELib;
using UELib.Core;

namespace SCModUELibBridge;

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
            return WriteFailure(outputUtf8, outputCapacity, error.Message);
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
        var patterns = new Dictionary<string, PatternCalls>(StringComparer.Ordinal);
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
                        if (OwningClassName(scriptObject) is { } className)
                        {
                            if (!patterns.TryGetValue(className, out var pattern))
                            {
                                pattern = new PatternCalls(className, Path.GetFileName(packagePath), []);
                                patterns.Add(className, pattern);
                            }
                            pattern.Calls.AddRange(DirectPairs.FindSoundCalls(script)
                                .Select(call => new PatternCall(sourceObject, call)));
                        }
                        foreach (var pair in DirectPairs.Find(script))
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

        var soundPackages = new Dictionary<string, SoundPackage?>(StringComparer.OrdinalIgnoreCase);
        SoundPackage? LoadSoundPackage(string name, string soundEvent)
        {
            if (soundPackages.TryGetValue(name, out var loaded))
            {
                return loaded;
            }

            SoundPackage? soundPackage = null;
            var path = FindPackagePath(soundsRoot, name + ".uax");
            if (path is null)
            {
                issues.Add($"Sound package was not found for {name}.{soundEvent}.");
            }
            else
            {
                try
                {
                    soundPackage = ReadSoundPackage(path);
                }
                catch (Exception error)
                {
                    issues.Add($"Could not load sound package {path}: {error.Message}");
                }
            }
            soundPackages.Add(name, soundPackage);
            return soundPackage;
        }

        var links = new List<BridgeLink>();
        foreach (var pair in sourcePairs)
        {
            if (LoadSoundPackage(pair.SoundPackage, pair.SoundEvent) is not { } soundPackage)
            {
                continue;
            }

            if (!soundPackage.Sounds.TryGetValue(pair.SoundEvent, out var sound))
            {
                issues.Add($"Sound object {soundPackage.Name}.{pair.SoundEvent} was not found in {soundPackage.Path}.");
                continue;
            }
            if (sound.Key is not { } key)
            {
                issues.Add($"Sound object {soundPackage.Name}.{pair.SoundEvent} uses an unsupported serialized key layout.");
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
        // Patterns whose sounds play without a subtitle; core matches them to
        // the lines of the pattern's localization section by position.
        var patternResults = new List<PatternResult>();
        foreach (var pattern in patterns.Values.Where(pattern => pattern.Calls.Any(call => call.Call.Pair is null)))
        {
            var calls = pattern.Calls.Select(call =>
            {
                var sound = LoadSoundPackage(call.Call.SoundPackage, call.Call.SoundEvent) is { } soundPackage
                    && soundPackage.Sounds.TryGetValue(call.Call.SoundEvent, out var entry)
                        ? entry.Key
                        : null;
                return new PatternCallResult(
                    call.SourceObject,
                    call.Call.SoundPackage + "." + call.Call.SoundEvent,
                    sound,
                    call.Call.Pair?.Section,
                    call.Call.Pair?.Cue,
                    call.Call.Function);
            });
            patternResults.Add(new PatternResult(pattern.ClassName, pattern.SourcePackage, calls.ToArray()));
        }

        return new BridgeResult(
            links,
            issues,
            soundPackages.Values.OfType<SoundPackage>().ToArray(),
            patternResults);
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

    private static string? OwningClassName(UObject item)
    {
        for (var outer = item.Outer; outer is not null; outer = outer.Outer)
        {
            if (outer is UClass)
            {
                return outer.Name;
            }
        }
        return null;
    }

    /// <summary>Reads every sound in a package, keeping each one's export position.</summary>
    private static SoundPackage ReadSoundPackage(string path)
    {
        var bytes = File.ReadAllBytes(path);
        using var package = UnrealLoader.LoadPackage(path, FileAccess.Read);
        package.InitializePackage(UnrealPackage.InitFlags.Construct | UnrealPackage.InitFlags.RegisterClasses);

        var sounds = new Dictionary<string, SoundEntry>(StringComparer.OrdinalIgnoreCase);
        for (var index = 0; index < package.Exports.Count; index++)
        {
            var export = package.Exports[index];
            if (export.Class?.ObjectName.ToString() != "Sound")
            {
                continue;
            }

            var name = export.ObjectName.ToString();
            uint? key = null;
            string? lipSyncPath = null;
            if (export.SerialOffset >= 0
                && export.SerialSize > 0
                && export.SerialOffset <= bytes.Length - export.SerialSize
                && SoundData.TryRead(
                    bytes.AsSpan(export.SerialOffset, export.SerialSize),
                    out var storedKey,
                    out lipSyncPath))
            {
                key = storedKey;
            }

            sounds.TryAdd(name, new SoundEntry(name, index, key, lipSyncPath));
        }

        return new SoundPackage(Path.GetFileNameWithoutExtension(path), path, sounds);
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
            writer.WritePropertyName("patterns");
            writer.WriteStartArray();
            foreach (var pattern in result.Patterns)
            {
                writer.WriteStartObject();
                writer.WriteString("pattern", pattern.ClassName);
                writer.WriteString("sourcePackage", pattern.SourcePackage);
                writer.WritePropertyName("calls");
                writer.WriteStartArray();
                foreach (var call in pattern.Calls)
                {
                    writer.WriteStartObject();
                    writer.WriteString("sourceObject", call.SourceObject);
                    writer.WriteString("soundEvent", call.SoundEvent);
                    if (call.SoundMapKey is { } key)
                    {
                        writer.WriteNumber("soundMapKey", key);
                    }
                    if (call.Section is not null && call.Cue is not null)
                    {
                        writer.WriteString("section", call.Section);
                        writer.WriteString("cue", call.Cue);
                    }
                    if (call.Function is not null)
                    {
                        writer.WriteString("function", call.Function);
                    }
                    writer.WriteEndObject();
                }
                writer.WriteEndArray();
                writer.WriteEndObject();
            }
            writer.WriteEndArray();
            writer.WritePropertyName("soundPackages");
            writer.WriteStartArray();
            foreach (var soundPackage in result.SoundPackages)
            {
                writer.WriteStartObject();
                writer.WriteString("package", soundPackage.Name);
                writer.WritePropertyName("sounds");
                writer.WriteStartArray();
                foreach (var sound in soundPackage.Sounds.Values.OrderBy(sound => sound.ExportIndex))
                {
                    if (sound.Key is not { } key)
                    {
                        continue;
                    }

                    writer.WriteStartObject();
                    writer.WriteString("name", sound.Name);
                    writer.WriteNumber("exportIndex", sound.ExportIndex);
                    writer.WriteNumber("soundMapKey", key);
                    if (sound.LipSyncPath is not null)
                    {
                        writer.WriteString("lipSyncPath", sound.LipSyncPath);
                    }
                    writer.WriteEndObject();
                }
                writer.WriteEndArray();
                writer.WriteEndObject();
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

    private sealed record BridgeResult(
        IReadOnlyList<BridgeLink> Links,
        IReadOnlyList<string> Issues,
        IReadOnlyList<SoundPackage> SoundPackages,
        IReadOnlyList<PatternResult> Patterns);

    private sealed record PatternCalls(string ClassName, string SourcePackage, List<PatternCall> Calls);

    private sealed record PatternCall(string SourceObject, SoundCall Call);

    private sealed record PatternResult(string ClassName, string SourcePackage, IReadOnlyList<PatternCallResult> Calls);

    private sealed record PatternCallResult(
        string SourceObject,
        string SoundEvent,
        uint? SoundMapKey,
        string? Section,
        string? Cue,
        string? Function);

    private sealed record SoundPackage(string Name, string Path, IReadOnlyDictionary<string, SoundEntry> Sounds);

    private sealed record SoundEntry(string Name, int ExportIndex, uint? Key, string? LipSyncPath);
}
