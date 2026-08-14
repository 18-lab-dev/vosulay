using System.Text;
using Microsoft.Diagnostics.NETCore.Client;

const string ProfilerClsid = "60f0ba7d-3ce1-4f69-9326-79a9e021b4a8";

if (args.Length != 6)
{
    Console.Error.WriteLine(
        "usage: vosulay-attach-profiler <pid> <profiler-so> <agent-so> <socket-hex> <cookie-hex> <host-pid>");
    return 2;
}

try
{
    var pid = ParsePositivePid(args[0]);
    var profilerPath = Path.GetFullPath(args[1]);
    var agentPath = Path.GetFullPath(args[2]);
    RequireRegularFile(profilerPath, "profiler library");
    RequireRegularFile(agentPath, "GL agent library");
    var socketName = DecodeHex(args[3], 1, 107, "socket name");
    var cookie = DecodeHex(args[4], 32, 32, "session cookie");
    var payload = BuildPayload(agentPath, socketName, cookie, ParsePositivePid(args[5]));
    var client = new DiagnosticsClient(pid);
    client.AttachProfiler(
        TimeSpan.FromSeconds(10),
        Guid.Parse(ProfilerClsid),
        profilerPath,
        payload);
    return 0;
}
catch (Exception error)
{
    Console.Error.WriteLine($"vosulay profiler attach failed: {error.Message}");
    return 1;
}

static int ParsePositivePid(string value)
{
    if (!int.TryParse(value, out var pid) || pid <= 0)
        throw new ArgumentException("target PID must be a positive integer");
    return pid;
}

static void RequireRegularFile(string path, string name)
{
    if (!File.Exists(path)) throw new FileNotFoundException($"{name} does not exist", path);
}

static byte[] DecodeHex(string value, int minBytes, int maxBytes, string name)
{
    if ((value.Length & 1) != 0 || value.Length < minBytes * 2 || value.Length > maxBytes * 2)
        throw new ArgumentException($"{name} has an invalid hexadecimal length");
    try
    {
        return Convert.FromHexString(value);
    }
    catch (FormatException)
    {
        throw new ArgumentException($"{name} is not hexadecimal");
    }
}

static byte[] BuildPayload(string agentPath, byte[] socketName, byte[] cookie, int hostPid)
{
    var agentPathBytes = Encoding.UTF8.GetBytes(agentPath);
    if (agentPathBytes.Contains((byte)0))
        throw new ArgumentException("GL agent path contains a NUL byte");
    var cookieHex = Encoding.ASCII.GetBytes(Convert.ToHexString(cookie).ToLowerInvariant());
    var hostPidBytes = Encoding.ASCII.GetBytes(hostPid.ToString());
    var payload = new byte[
        agentPathBytes.Length + socketName.Length + cookieHex.Length + hostPidBytes.Length + 4];
    var offset = 0;
    agentPathBytes.CopyTo(payload, offset);
    offset += agentPathBytes.Length + 1;
    socketName.CopyTo(payload, offset);
    offset += socketName.Length + 1;
    cookieHex.CopyTo(payload, offset);
    offset += cookieHex.Length + 1;
    hostPidBytes.CopyTo(payload, offset);
    return payload;
}
