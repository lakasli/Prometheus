let coreLoad: Promise<void> | undefined;

function loadCore(path: string): Promise<void> {
  if (coreLoad) return coreLoad;
  const url = new URL(path, location.href);
  if (url.origin !== location.origin)
    throw new Error("Cubism Core 必须从本机加载。");

  coreLoad = new Promise<void>((resolve, reject) => {
    const script = document.createElement("script");
    script.src = url.href;
    script.onload = () => {
      script.remove();
      if ("Live2DCubismCore" in window) resolve();
      else reject(new Error("Cubism Core 脚本未初始化。"));
    };
    script.onerror = () => {
      script.remove();
      reject(new Error("无法加载 Cubism Core。请检查本机 SDK 路径。"));
    };
    document.head.append(script);
  }).catch((error: unknown) => {
    coreLoad = undefined;
    throw error;
  });
  return coreLoad;
}

export async function ensureCubismCore(
  path: string,
  signal: AbortSignal,
): Promise<void> {
  signal.throwIfAborted();
  await loadCore(path);
  const deadline = performance.now() + 15_000;
  // The Core global exists before its embedded runtime finishes initializing.
  for (;;) {
    signal.throwIfAborted();
    try {
      const core = window as unknown as {
        Live2DCubismCore: { Version: { csmGetVersion(): number } };
      };
      core.Live2DCubismCore.Version.csmGetVersion();
      return;
    } catch {
      if (performance.now() >= deadline) {
        throw new Error("Cubism Core 运行时初始化超时。");
      }
      await new Promise<void>((resolve) => setTimeout(resolve, 16));
    }
  }
}
