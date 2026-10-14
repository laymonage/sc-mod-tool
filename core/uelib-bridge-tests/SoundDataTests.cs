using System.Text;
using SCModUELibBridge;

namespace UELibBridge.Tests;

public class SoundDataTests
{
    [Fact]
    public void ReadsTheKeyOfASoundWithoutLipSync()
    {
        byte[] data = [0x00, 0x01, 0x00, 0x3C, 0x00, 0, 0, 0, 0, 0];

        Assert.True(SoundData.TryRead(data, out var key, out var lipSyncPath));
        Assert.Equal(0x003C0001u, key);
        Assert.Null(lipSyncPath);
    }

    [Fact]
    public void ReadsTheLipSyncPathOfALipSyncedSound()
    {
        var path = @"\S0_0_2Voice\00_15_01.bin";
        byte[] data =
        [
            0x00, 0x29, 0x00, 0x3C, 0x00,
            0x01, 0x00, 0x00, 0x00,
            (byte)(path.Length + 1),
            .. Encoding.ASCII.GetBytes(path),
            0x00,
        ];

        Assert.True(SoundData.TryRead(data, out var key, out var lipSyncPath));
        Assert.Equal(0x003C0029u, key);
        Assert.Equal(path, lipSyncPath);
    }

    [Fact]
    public void ReadsTheKeyOfAShortRecord()
    {
        Assert.True(SoundData.TryRead([0x00, 0x05, 0x00, 0x82, 0x00], out var key, out var lipSyncPath));
        Assert.Equal(0x00820005u, key);
        Assert.Null(lipSyncPath);
    }

    [Theory]
    [InlineData(new byte[] { 0x00, 0x01, 0x00 })]
    [InlineData(new byte[] { 0x05, 0x01, 0x00, 0x3C, 0x00 })]
    public void RejectsUnsupportedLayouts(byte[] data)
    {
        Assert.False(SoundData.TryRead(data, out _, out _));
    }

    [Fact]
    public void IgnoresATruncatedLipSyncPath()
    {
        byte[] data = [0x00, 0x01, 0x00, 0x3C, 0x00, 0x01, 0x00, 0x00, 0x00, 0x1A, (byte)'\\', (byte)'S'];

        Assert.True(SoundData.TryRead(data, out var key, out var lipSyncPath));
        Assert.Equal(0x003C0001u, key);
        Assert.Null(lipSyncPath);
    }
}
