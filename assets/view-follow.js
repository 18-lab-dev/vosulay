function (profileId, stateUrl) {
  // Explicit ?profile= pages never install this follower. The host owns ruleset
  // selection, so this also follows manual switches without competing writers.
  let pending = false;
  setInterval(async () => {
    if (pending) return;
    pending = true;
    try {
      const response = await fetch(stateUrl, { cache: 'no-store', credentials: 'omit' });
      if (response.ok && (await response.json()).active_profile !== profileId) location.reload();
    } catch (_) { /* Keep the last view while the host is unavailable. */ }
    finally { pending = false; }
  }, 1000);
}
