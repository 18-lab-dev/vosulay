# Stable / Wine GLX DMA-BUF validation

Use this after building the current PR branch to validate the zero-copy path on osu!stable/Wine. Record the GPU and driver version, Wine build, DXVK version, desktop session (X11 or XWayland), and the output of `glxinfo -B` alongside the test result so import failures can be reproduced.

## Expected zero-copy path

Run vosulay with native diagnostics enabled and attach to the stable client normally:

```sh
VOSULAY_NATIVE_DIAGNOSTICS=1 vosulay --pid <stable-pid>
```

The GL agent should report all of the following:

```text
vosulay target transport: dmabuf=true
vosulay GL agent: DMA-BUF import path: GL_EXT_memory_object_fd
vosulay DMA-BUF compositor first frame: drawn (gpu fencing: enabled)
```

`DMA-BUF import path: EGLImage` is not expected for Wine/GLX. The GLX compositor deliberately does not fall back to EGLImage because that path can bind successfully and still sample black.

## Fallback behavior

If the real Chromium native-pixmap FD cannot be imported through `GL_EXT_memory_object_fd`, the agent should withdraw DMA-BUF, disconnect the session, and let the overlay manager recreate the offscreen BrowserWindow with the SHM transport. The game must remain intact throughout the transition.

Force the compatibility path for comparison with:

```sh
VOSULAY_GL_SHM_ONLY=1 VOSULAY_NATIVE_DIAGNOSTICS=1 vosulay --pid <stable-pid>
```

That run should report `dmabuf=false` and the shared-memory compositor first frame.

## Lifetime stress

For the DMA-BUF run:

1. Leave the same overlay frame visible across hundreds of game presents.
2. Trigger rapid overlay updates, resize the game window, and switch focus/targets.
3. Exercise 144 Hz, 240 Hz, 480 Hz, and uncapped presentation if available.
4. Confirm each Chromium texture is released only after its final-use GL fence completes; there must be no duplicate consumed events, stalls from normal fence waits, black overlay frames, or game-frame corruption.
5. Hide/show and destroy the overlay while GPU work is in flight; teardown must not recycle a sampled DMA-BUF early.

If direct native-pixmap import succeeds but produces layout corruption, the next fallback to implement is a GBM renderable staging BO + GPU blit, matching the normalization path used by `K4zoku/idk-overlay`.
