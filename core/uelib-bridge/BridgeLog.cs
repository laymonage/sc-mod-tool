using UELib.Services;

namespace SCModUELibBridge;

/// <summary>
/// Collects the errors UELib recovers from, which its default log service
/// prints to stderr without saying which object they came from.
/// </summary>
internal sealed class BridgeLog : ILogService
{
    private readonly List<string> errors = [];

    public void Log(string text)
    {
    }

    public void Log(string format, params object?[] arg)
    {
    }

    public void SilentException(Exception exception) => errors.Add($"{exception.GetType().Name}: {exception.Message}");

    public void SilentAssert(bool assert, string message)
    {
        if (!assert)
        {
            errors.Add(message);
        }
    }

    /// <summary>Returns the errors logged since the last call, then forgets them.</summary>
    internal string[] Drain()
    {
        var logged = errors.Distinct().ToArray();
        errors.Clear();
        return logged;
    }
}
