using System.Buffers.Binary;
using System.Text;

namespace SCModUELibBridge;

/// <summary>Reads the serialized data of a Splinter Cell <c>Engine.Sound</c> export.</summary>
internal static class SoundData
{
    /// <summary>
    /// Reads the sound's map key and, for lip-synced sounds, the path of its
    /// lip-sync file.
    /// </summary>
    /// <remarks>
    /// The layout is an empty property list (one zero byte), the map key, then
    /// optionally a lip-sync flag and a length-prefixed, null-terminated path such
    /// as <c>\S0_0_2Voice\00_15_01.bin</c>.
    /// </remarks>
    internal static bool TryRead(ReadOnlySpan<byte> data, out uint key, out string? lipSyncPath)
    {
        key = 0;
        lipSyncPath = null;
        if (data.Length < 5 || data[0] != 0)
        {
            return false;
        }

        key = BinaryPrimitives.ReadUInt32LittleEndian(data[1..]);
        if (data.Length < 10 || BinaryPrimitives.ReadUInt32LittleEndian(data[5..]) != 1)
        {
            return true;
        }

        // Short strings store their length, including the terminator, in one byte.
        var length = data[9];
        if (length is > 1 and < 64 && data.Length >= 10 + length && data[9 + length] == 0)
        {
            lipSyncPath = Encoding.Latin1.GetString(data.Slice(10, length - 1));
        }

        return true;
    }
}
