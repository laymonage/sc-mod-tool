using SCModUELibBridge;

namespace UELibBridge.Tests;

public class DirectPairsTests
{
    private static DirectPair[] Find(string script) => DirectPairs.Find(script).ToArray();

    [Fact]
    public void FindsALocalizedCuePassedWithItsSound()
    {
        var pairs = Find(
            """SendTransmissionMessage(Localize("LambertWarnings", "Speech_0001L", "Hint"), Sound'Lambert.Play_AlarmStage1Opt1');""");

        Assert.Equal(
            [new DirectPair("LambertWarnings", "Speech_0001L", "Hint", "Lambert", "Play_AlarmStage1Opt1")],
            pairs);
    }

    [Fact]
    public void AllowsSingleQuotesAndWhitespaceBetweenArguments()
    {
        var pairs = Find("Localize ( 'Pilot' ,\n\t'Speech_0002L' , 'Lines' ) ,\n  Sound'S1_1_1Voice.Play_11_25_02'");

        Assert.Equal(
            [new DirectPair("Pilot", "Speech_0002L", "Lines", "S1_1_1Voice", "Play_11_25_02")],
            pairs);
    }

    [Fact]
    public void FindsEveryPairInAScript()
    {
        var pairs = Find(
            """
            Speak(Localize("A", "One", "Lines"), Sound'Pkg.First');
            if (bAlarm)
            {
                Speak(Localize("B", "Two", "Lines"), Sound'Pkg.Second');
            }
            """);

        Assert.Equal(["First", "Second"], pairs.Select(pair => pair.SoundEvent));
    }

    [Fact]
    public void SplitsTheSoundReferenceAtTheLastDot()
    {
        var pair = Assert.Single(Find("""Localize("A", "B", "C"), Sound'Pkg.Group.Event'"""));

        Assert.Equal("Pkg.Group", pair.SoundPackage);
        Assert.Equal("Event", pair.SoundEvent);
    }

    [Fact]
    public void UnescapesQuotesAndBackslashesButKeepsOtherEscapes()
    {
        var pair = Assert.Single(Find("""Localize("A", "Say \"hi\" \\ now\n", "C"), Sound'Pkg.Event'"""));

        Assert.Equal("""Say "hi" \ now\n""", pair.Cue);
    }

    [Theory]
    [InlineData("""MyLocalize("A", "B", "C"), Sound'Pkg.Event'""")]
    [InlineData("""LocalizeText("A", "B", "C"), Sound'Pkg.Event'""")]
    public void IgnoresOtherFunctionsWhoseNamesContainLocalize(string script)
    {
        Assert.Empty(Find(script));
    }

    [Theory]
    [InlineData("""Speak(Localize("A", "B", "C"));""")]
    [InlineData("""Speak(Localize("A", "B", "C"), 2.0, Sound'Pkg.Event');""")]
    [InlineData("""Speak(Localize("A", "B", "C"), SoundCue'Pkg.Event');""")]
    [InlineData("""Speak(Localize("A", "B", "C"), Sound"Pkg.Event");""")]
    public void IgnoresCuesThatArentFollowedDirectlyByASound(string script)
    {
        Assert.Empty(Find(script));
    }

    [Theory]
    [InlineData("""Localize("A", "B"), Sound'Pkg.Event'""")]
    [InlineData("""Localize("A", "B", Name), Sound'Pkg.Event'""")]
    [InlineData("""Localize("A", "B", "C", "D"), Sound'Pkg.Event'""")]
    public void IgnoresLocalizeCallsWithoutThreeStringArguments(string script)
    {
        Assert.Empty(Find(script));
    }

    [Theory]
    [InlineData("""Localize("A", "B", "C"), Sound'Event'""")]
    [InlineData("""Localize("A", "B", "C"), Sound'.Event'""")]
    [InlineData("""Localize("A", "B", "C"), Sound'Pkg.'""")]
    public void IgnoresSoundReferencesWithoutAPackageAndEvent(string script)
    {
        Assert.Empty(Find(script));
    }

    [Theory]
    [InlineData("")]
    [InlineData("Localize")]
    [InlineData("""Localize("A""")]
    [InlineData("""Localize("A", "B", "C"), Sound'Pkg.Event""")]
    public void StopsCleanlyOnTruncatedScripts(string script)
    {
        Assert.Empty(Find(script));
    }
}
