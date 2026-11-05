import { CubismModelSettingJson } from "@cubism/cubismmodelsettingjson";
import { CubismEyeBlink } from "@cubism/effect/cubismeyeblink";
import { CubismFramework } from "@cubism/live2dcubismframework";
import { CubismMatrix44 } from "@cubism/math/cubismmatrix44";
import { CubismUserModel } from "@cubism/model/cubismusermodel";
import { ACubismMotion } from "@cubism/motion/acubismmotion";
import { CubismWebGLOffscreenManager } from "@cubism/rendering/cubismoffscreenmanager";
import { CubismShaderManager_WebGL } from "@cubism/rendering/cubismshader_webgl";

type TextureSize = { width: number; height: number };

export type StageMetrics = {
  fps: number;
  frames: number;
  maxTextureSize: number;
  textures: readonly TextureSize[];
  firstFrameMs: number;
};

export type StageOptions = {
  modelUrl: string;
  startedAt: number;
  idlePath: string;
  expressions: Readonly<Record<string, string>>;
  onMetrics?: (metrics: StageMetrics) => void;
  onFatalError?: (message: string) => void;
  signal?: AbortSignal;
};

export type StageRenderer = {
  setZoom(value: number): void;
  setExpression(id: string | null): void;
  dispose(): void;
};

const shaderPath = `${import.meta.env.BASE_URL}assets/cubism-shaders/`;
let stageActive = false;

async function fetchAsset(url: URL, signal: AbortSignal): Promise<ArrayBuffer> {
  const response = await fetch(url, { signal, redirect: "error" });
  if (!response.ok) {
    const name = decodeURIComponent(url.pathname.split("/").at(-1) ?? "");
    throw new Error(`无法读取 ${name}（HTTP ${response.status}）。`);
  }
  return response.arrayBuffer();
}

async function uploadTexture(
  gl: WebGL2RenderingContext,
  url: URL,
  maxTextureSize: number,
  signal: AbortSignal,
): Promise<TextureSize & { texture: WebGLTexture }> {
  const bytes = await fetchAsset(url, signal);
  const header = new DataView(bytes);
  if (
    header.byteLength < 24 ||
    header.getUint32(0) !== 0x89504e47 ||
    header.getUint32(4) !== 0x0d0a1a0a ||
    header.getUint32(12) !== 0x49484452
  ) {
    throw new Error("模型贴图的 PNG 文件头无效。");
  }
  const width = header.getUint32(16);
  const height = header.getUint32(20);
  if (!width || !height || Math.max(width, height) > maxTextureSize) {
    throw new Error(
      `原尺寸贴图 ${width} × ${height} 超出 WebGL 纹理上限 ${maxTextureSize}。`,
    );
  }

  const bitmap = await createImageBitmap(
    new Blob([bytes], { type: "image/png" }),
    {
      premultiplyAlpha: "premultiply",
      colorSpaceConversion: "none",
    },
  );
  try {
    signal.throwIfAborted();
    if (bitmap.width !== width || bitmap.height !== height) {
      throw new Error("浏览器解码后的贴图尺寸与模型原图不符。");
    }
    const texture = gl.createTexture();
    if (!texture) throw new Error("无法分配 WebGL 纹理。");

    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, bitmap);
    gl.bindTexture(gl.TEXTURE_2D, null);
    const error = gl.getError();
    if (error !== gl.NO_ERROR) {
      gl.deleteTexture(texture);
      throw new Error(
        `无法上传原尺寸贴图 ${width} × ${height}（WebGL ${error}）。`,
      );
    }
    return { width, height, texture };
  } finally {
    bitmap.close();
  }
}

class StageModel extends CubismUserModel {
  private idle?: ACubismMotion;
  private readonly expressions = new Map<string, ACubismMotion>();
  private readonly projection = new CubismMatrix44();

  setup(
    setting: CubismModelSettingJson,
    moc: ArrayBuffer,
    physics: ArrayBuffer,
    idle: ArrayBuffer,
    expressions: ReadonlyArray<readonly [string, ArrayBuffer]>,
  ): void {
    this.loadModel(moc, true);
    if (!this.getModel()) throw new Error("Cubism Core 无法载入模型。");

    this.loadPhysics(physics, physics.byteLength);
    this._eyeBlink = CubismEyeBlink.create(setting);
    this._physics?.stabilization(this._model);

    const idleMotion = this.loadMotion(idle, idle.byteLength, "idle");
    if (!idleMotion) throw new Error("无法载入待机动画。");
    idleMotion.setEffectIds(
      Array.from({ length: setting.getEyeBlinkParameterCount() }, (_, index) =>
        setting.getEyeBlinkParameterId(index),
      ),
      Array.from({ length: setting.getLipSyncParameterCount() }, (_, index) =>
        setting.getLipSyncParameterId(index),
      ),
    );
    idleMotion.setLoop(true);
    this.idle = idleMotion;
    this._motionManager.startMotionPriority(idleMotion, false, 1);

    for (const [id, data] of expressions) {
      const motion = this.loadExpression(data, data.byteLength, id);
      if (!motion) throw new Error(`无法载入表情 ${id}。`);
      this.expressions.set(id, motion);
    }
    this._model.update();
  }

  setExpression(id: string | null): void {
    this._expressionManager.stopAllMotions();
    const expression = id === null ? undefined : this.expressions.get(id);
    if (expression) this._expressionManager.startMotion(expression, false);
  }

  update(delta: number): void {
    this._model.loadParameters();
    this._motionManager.updateMotion(this._model, delta);
    this._model.saveParameters();
    this._eyeBlink?.updateParameters(this._model, delta);
    this._expressionManager.updateMotion(this._model, delta);
    this._physics?.evaluate(this._model, delta);
    this._model.update();
  }

  measureBounds(): {
    left: number;
    right: number;
    bottom: number;
    top: number;
  } {
    let left = Infinity;
    let right = -Infinity;
    let bottom = Infinity;
    let top = -Infinity;
    for (let index = 0; index < this._model.getDrawableCount(); index++) {
      if (
        !this._model.getDrawableDynamicFlagIsVisible(index) ||
        this._model.getDrawableOpacity(index) <= 0.01
      )
        continue;

      const vertices = this._model.getDrawableVertices(index);
      for (let vertex = 0; vertex < vertices.length; vertex += 2) {
        left = Math.min(left, vertices[vertex]!);
        right = Math.max(right, vertices[vertex]!);
        bottom = Math.min(bottom, vertices[vertex + 1]!);
        top = Math.max(top, vertices[vertex + 1]!);
      }
    }
    if (!(right > left && top > bottom))
      throw new Error("无法计算模型可见范围。");
    return { left, right, bottom, top };
  }

  draw(
    width: number,
    height: number,
    zoom: number,
    bounds: ReturnType<StageModel["measureBounds"]>,
  ): void {
    const pixelsPerUnit =
      0.9 *
      zoom *
      Math.min(
        width / (bounds.right - bounds.left),
        height / (bounds.top - bounds.bottom),
      );
    const scaleX = (2 * pixelsPerUnit) / width;
    const scaleY = (2 * pixelsPerUnit) / height;
    this.projection.scale(scaleX, scaleY);
    this.projection.translate(
      (-(bounds.left + bounds.right) * scaleX) / 2,
      (-(bounds.bottom + bounds.top) * scaleY) / 2,
    );

    const renderer = this.getRenderer();
    renderer.setMvpMatrix(this.projection);
    // The Cubism declaration omits null. WebGL uses it for the canvas framebuffer.
    renderer.setRenderState(null as unknown as WebGLFramebuffer, [
      0,
      0,
      width,
      height,
    ]);
    renderer.drawModel(shaderPath);
  }

  override release(): void {
    this._expressionManager.stopAllMotions();
    super.release();
    if (this.idle) ACubismMotion.delete(this.idle);
    for (const expression of this.expressions.values())
      ACubismMotion.delete(expression);
    this.expressions.clear();
  }
}

async function waitForShaders(
  gl: WebGL2RenderingContext,
  signal: AbortSignal,
): Promise<void> {
  const shader = CubismShaderManager_WebGL.getInstance().getShader(gl);
  const deadline = performance.now() + 15_000;
  // R5 generates its shader programs asynchronously and exposes readiness on the shader object.
  while (!shader._isShaderLoaded) {
    signal.throwIfAborted();
    if (
      !shader._isShaderLoading ||
      gl.isContextLost() ||
      performance.now() > deadline
    ) {
      throw new Error("Cubism WebGL 着色器初始化失败。");
    }
    await new Promise<void>((resolve) => setTimeout(resolve, 16));
  }
  if (
    !shader._shaderSets[1]?.shaderProgram ||
    !shader._shaderSets[2]?.shaderProgram
  ) {
    throw new Error("Cubism WebGL 绘制或遮罩着色器不可用。");
  }
}

export async function createCubismStage(
  canvas: HTMLCanvasElement,
  options: StageOptions,
): Promise<StageRenderer> {
  options.signal?.throwIfAborted();
  if (stageActive) throw new Error("当前页面已开启一个 Live2D 舞台。");
  const context = canvas.getContext("webgl2", {
    alpha: true,
    premultipliedAlpha: true,
  });
  if (!context) throw new Error("浏览器未能初始化 WebGL 2。");
  const gl = context;
  stageActive = true;

  const controller = new AbortController();
  const signal = options.signal
    ? AbortSignal.any([controller.signal, options.signal])
    : controller.signal;
  const textures: WebGLTexture[] = [];
  const textureSizes: TextureSize[] = [];
  const maxTextureSize: number = gl.getParameter(gl.MAX_TEXTURE_SIZE);
  const maxViewport: Int32Array = gl.getParameter(gl.MAX_VIEWPORT_DIMS);
  let resize: ResizeObserver | undefined;
  let model: StageModel | undefined;
  let frame = 0;
  let zoom = 1;
  let cssWidth = canvas.clientWidth;
  let cssHeight = canvas.clientHeight;
  let disposed = false;
  let initialized = false;

  function onContextLost(event: Event): void {
    event.preventDefault();
    const message = "WebGL 上下文已丢失。请重新打开舞台。";
    if (initialized) {
      dispose();
      options.onFatalError?.(message);
    } else {
      controller.abort(new Error(message));
    }
  }

  function dispose(): void {
    if (disposed) return;
    disposed = true;
    controller.abort();
    cancelAnimationFrame(frame);
    resize?.disconnect();
    canvas.removeEventListener("webglcontextlost", onContextLost);
    options.signal?.removeEventListener("abort", dispose);
    try {
      model?.release();
      for (const texture of textures) gl.deleteTexture(texture);
      CubismWebGLOffscreenManager.getInstance().removeContext(gl);
      if (CubismFramework.isInitialized()) CubismFramework.dispose();
    } finally {
      stageActive = false;
    }
  }

  canvas.addEventListener("webglcontextlost", onContextLost);
  try {
    const modelUrl = new URL(options.modelUrl, location.href);
    const modelRoot = new URL(".", modelUrl);
    if (modelRoot.origin !== location.origin)
      throw new Error("模型必须从本机加载。");
    const assetUrl = (path: string) => {
      const url = new URL(path, modelRoot);
      if (!path || !url.href.startsWith(modelRoot.href))
        throw new Error("模型资源路径超出目录。");
      return url;
    };
    const getAsset = (path: string) => fetchAsset(assetUrl(path), signal);
    const manifest = await fetchAsset(modelUrl, signal);
    if (!CubismFramework.startUp())
      throw new Error("Cubism Framework 启动失败。");
    CubismFramework.initialize();
    const setting = new CubismModelSettingJson(manifest, manifest.byteLength);
    try {
      const [moc, physics, idle, expressions] = await Promise.all([
        getAsset(setting.getModelFileName()),
        getAsset(setting.getPhysicsFileName()),
        getAsset(options.idlePath),
        Promise.all(
          Object.entries(options.expressions).map(
            async ([id, path]) => [id, await getAsset(path)] as const,
          ),
        ),
      ]);
      signal.throwIfAborted();
      model = new StageModel();
      model.setup(setting, moc, physics, idle, expressions);
      model.createRenderer(Math.max(1, cssWidth), Math.max(1, cssHeight));
      const renderer = model.getRenderer();
      renderer.startUp(gl);
      renderer.setIsPremultipliedAlpha(true);
      renderer.loadShaders(shaderPath);

      // Decode and upload one atlas at a time to limit peak browser memory.
      for (let index = 0; index < setting.getTextureCount(); index++) {
        const { width, height, texture } = await uploadTexture(
          gl,
          assetUrl(setting.getTextureFileName(index)),
          maxTextureSize,
          signal,
        );
        textures.push(texture);
        textureSizes.push({ width, height });
        renderer.bindTexture(index, texture);
      }
      if (!textures.length) throw new Error("模型清单没有提供贴图。");
      await waitForShaders(gl, signal);
    } finally {
      setting.release();
    }
    signal.throwIfAborted();

    const readyModel = model;
    const bounds = readyModel.measureBounds();
    const offscreen = CubismWebGLOffscreenManager.getInstance();
    let frameCount = 0;
    let intervalFrames = 0;
    let lastFrameTime = 0;
    let measureStart = 0;
    let firstFrameMs = 0;

    function draw(time: number): void {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const width = Math.min(
        maxViewport[0]!,
        maxTextureSize,
        Math.round(cssWidth * dpr),
      );
      const height = Math.min(
        maxViewport[1]!,
        maxTextureSize,
        Math.round(cssHeight * dpr),
      );
      if (!width || !height) return;

      if (!frameCount || width !== canvas.width || height !== canvas.height) {
        canvas.width = width;
        canvas.height = height;
        offscreen.initialize(gl, width, height);
      }
      const delta = lastFrameTime
        ? Math.max(0, Math.min((time - lastFrameTime) / 1_000, 0.066))
        : 1 / 60;
      lastFrameTime = time;
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      gl.viewport(0, 0, width, height);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      offscreen.beginFrameProcess(gl);
      try {
        readyModel.update(delta);
        readyModel.draw(width, height, zoom, bounds);
      } finally {
        offscreen.endFrameProcess(gl);
        offscreen.releaseStaleRenderTextures(gl);
      }
      frameCount++;
    }

    function report(fps: number): void {
      options.onMetrics?.({
        fps,
        frames: frameCount,
        maxTextureSize,
        textures: textureSizes,
        firstFrameMs,
      });
    }

    function animate(time: number): void {
      if (disposed) return;
      try {
        const before = frameCount;
        draw(time);
        if (frameCount > before) {
          intervalFrames++;
          if (!firstFrameMs) {
            firstFrameMs = performance.now() - options.startedAt;
            report(0);
          }
          if (!measureStart) {
            measureStart = time;
            intervalFrames = 0;
          } else if (time - measureStart >= 1_000) {
            report((1_000 * intervalFrames) / (time - measureStart));
            measureStart = time;
            intervalFrames = 0;
          }
        }
        frame = requestAnimationFrame(animate);
      } catch (error) {
        dispose();
        options.onFatalError?.(
          error instanceof Error ? error.message : "Live2D 绘制失败。",
        );
      }
    }

    resize = new ResizeObserver(([entry]) => {
      if (!entry) return;
      cssWidth = entry.contentRect.width;
      cssHeight = entry.contentRect.height;
    });
    resize.observe(canvas);
    draw(performance.now());
    if (gl.getError() !== gl.NO_ERROR) throw new Error("WebGL 首帧绘制失败。");
    if (frameCount) {
      firstFrameMs = performance.now() - options.startedAt;
      report(0);
    }
    function updateVisibility(): void {
      cancelAnimationFrame(frame);
      lastFrameTime = 0;
      measureStart = 0;
      intervalFrames = 0;
      if (!document.hidden) frame = requestAnimationFrame(animate);
    }

    document.addEventListener("visibilitychange", updateVisibility, {
      signal: controller.signal,
    });
    updateVisibility();
    initialized = true;
    options.signal?.addEventListener("abort", dispose, { once: true });
    return {
      setZoom(value) {
        if (Number.isFinite(value)) zoom = Math.min(1.8, Math.max(0.6, value));
      },
      setExpression(id) {
        if (!disposed) readyModel.setExpression(id);
      },
      dispose,
    };
  } catch (error) {
    dispose();
    throw error;
  }
}
