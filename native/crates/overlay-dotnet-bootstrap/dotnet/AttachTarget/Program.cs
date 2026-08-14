using System.Runtime.InteropServices;

if (args.Length != 1)
{
    Console.Error.WriteLine("usage: AttachTarget <ready-file>");
    return 2;
}

var library = NativeLibrary.Load(Path.Combine(AppContext.BaseDirectory, "libvosulay-egl-target.so"));
var entry = NativeLibrary.GetExport(library, "vosulay_run_egl_target");
var run = Marshal.GetDelegateForFunctionPointer<RunEglTarget>(entry);
Console.WriteLine(Environment.ProcessId);
Console.Out.Flush();
return run(Path.GetFullPath(args[0]));

[UnmanagedFunctionPointer(CallingConvention.Cdecl)]
delegate int RunEglTarget([MarshalAs(UnmanagedType.LPUTF8Str)] string readyPath);
