import { rampPixels } from "./ramp";

/**
 * Draws heat sources for one frame, in CSS pixels of the viewport.
 * Red holds steady targets (the field warms toward them); green holds
 * one-shot impulses (added once, then left to cool).
 */
export class Painter {
  readonly canvas = document.createElement("canvas");
  private readonly ctx: CanvasRenderingContext2D;
  private scale = 1;

  constructor() {
    const ctx = this.canvas.getContext("2d", { willReadFrequently: false });
    if (!ctx) throw new Error("2d context unavailable");
    this.ctx = ctx;
  }

  resize(width: number, height: number): void {
    this.canvas.width = width;
    this.canvas.height = height;
  }

  begin(cssWidth: number): void {
    const { ctx, canvas } = this;
    this.scale = canvas.width / cssWidth;
    ctx.globalCompositeOperation = "source-over";
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.globalCompositeOperation = "lighter";
  }

  /** A steady warm body: the field under it approaches `temp` (0 to 1). */
  spot(x: number, y: number, radius: number, temp: number): void {
    this.blob(x, y, radius, temp, "255,0,0");
  }

  /** A one-shot deposit of heat, such as a touch. */
  pulse(x: number, y: number, radius: number, amount: number): void {
    this.blob(x, y, radius, amount, "0,255,0");
  }

  /** Deposits heat along a segment, in proportion to its length. */
  streak(x1: number, y1: number, x2: number, y2: number, radius: number, amount: number): void {
    const dist = Math.hypot(x2 - x1, y2 - y1);
    const step = radius * 0.45;
    const n = Math.max(1, Math.ceil(dist / step));
    const each = dist < 1 ? amount * 2 : (amount * dist) / (n * step);
    for (let i = 1; i <= n; i++) {
      const k = i / n;
      this.pulse(x1 + (x2 - x1) * k, y1 + (y2 - y1) * k, radius, each);
    }
  }

  /** A steady warm line between two points. */
  line(x1: number, y1: number, x2: number, y2: number, width: number, temp: number): void {
    const { ctx, scale } = this;
    ctx.strokeStyle = `rgba(255,0,0,${clamp01(temp)})`;
    ctx.lineWidth = Math.max(1, width * scale);
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(x1 * scale, y1 * scale);
    ctx.lineTo(x2 * scale, y2 * scale);
    ctx.stroke();
  }

  private blob(x: number, y: number, radius: number, level: number, rgb: string): void {
    const a = clamp01(level);
    if (a <= 0.002 || radius <= 0) return;
    const { ctx, scale } = this;
    const cx = x * scale;
    const cy = y * scale;
    const r = Math.max(1.5, radius * scale);
    if (cx < -r || cy < -r || cx > this.canvas.width + r || cy > this.canvas.height + r) return;
    const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, r);
    // Close to a Gaussian: a small hot center, a long cool skirt.
    g.addColorStop(0, `rgba(${rgb},${a})`);
    g.addColorStop(0.14, `rgba(${rgb},${a * 0.9})`);
    g.addColorStop(0.3, `rgba(${rgb},${a * 0.66})`);
    g.addColorStop(0.5, `rgba(${rgb},${a * 0.34})`);
    g.addColorStop(0.7, `rgba(${rgb},${a * 0.12})`);
    g.addColorStop(0.86, `rgba(${rgb},${a * 0.03})`);
    g.addColorStop(1, `rgba(${rgb},0)`);
    ctx.fillStyle = g;
    ctx.fillRect(cx - r, cy - r, r * 2, r * 2);
  }
}

function clamp01(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

const VERT = `#version 300 es
in vec2 a_pos;
out vec2 v_uv;
void main() {
  v_uv = a_pos * 0.5 + 0.5;
  gl_Position = vec4(a_pos, 0.0, 1.0);
}`;

const NOISE = `
float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}
float noise(vec2 p) {
  vec2 i = floor(p), f = fract(p);
  f = f * f * (3.0 - 2.0 * f);
  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x),
             mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);
}`;

/** One step of the heat field: carry, rise, spread, cool, then take in sources. */
const SIM_FRAG = `#version 300 es
precision highp float;
uniform sampler2D u_prev;
uniform sampler2D u_src;
uniform vec2 u_texel;
uniform vec2 u_up;
uniform float u_scroll;
uniform float u_dt;
uniform float u_time;
uniform float u_aspect;
uniform float u_motion;
in vec2 v_uv;
out vec4 o;
${NOISE}
float T(vec2 p) {
  vec2 inside = step(vec2(0.0), p) * step(p, vec2(1.0));
  return textureLod(u_prev, p, 0.0).r * inside.x * inside.y;
}
void main() {
  vec2 base = v_uv - vec2(0.0, u_scroll);
  float here = T(base);
  vec2 q = vec2(base.x * u_aspect, base.y) * 5.0;
  vec2 swirl = vec2(noise(q + vec2(0.0, -u_time * 0.6)),
                    noise(q.yx + vec2(11.3, u_time * 0.5))) - 0.5;
  vec2 vel = (u_up * (0.003 + 0.034 * here) + swirl * 0.018 * smoothstep(0.02, 0.4, here)) * u_motion;
  vec2 p = base - vec2(vel.x / u_aspect, vel.y) * u_dt;
  float c = T(p);
  float around = 0.25 * (T(p + vec2(u_texel.x, 0.0)) + T(p - vec2(u_texel.x, 0.0)) +
                         T(p + vec2(0.0, u_texel.y)) + T(p - vec2(0.0, u_texel.y)));
  float t = mix(c, around, 0.4);
  t *= exp(-u_dt / 2.4);
  t = max(t - 0.012 * u_dt, 0.0);
  vec4 s = texture(u_src, v_uv);
  t += max(s.r - t, 0.0) * (1.0 - exp(-u_dt * 7.0));
  t += s.g;
  o = vec4(min(t, 1.12), 0.0, 0.0, 1.0);
}`;

/** Temperature to color: the ramp, isotherm lines and sensor grain. */
const SHOW_FRAG = `#version 300 es
precision highp float;
uniform sampler2D u_field;
uniform sampler2D u_ramp;
uniform vec2 u_sim;
uniform vec2 u_res;
uniform float u_time;
uniform float u_offset;
in vec2 v_uv;
out vec4 o;
${NOISE}
vec4 cubic(float v) {
  vec4 n = vec4(1.0, 2.0, 3.0, 4.0) - v;
  vec4 s = n * n * n;
  float x = s.x;
  float y = s.y - 4.0 * s.x;
  float z = s.z - 4.0 * s.y + 6.0 * s.x;
  float w = 6.0 - x - y - z;
  return vec4(x, y, z, w) * (1.0 / 6.0);
}
float field(vec2 uv) {
  vec2 st = uv * u_sim - 0.5;
  vec2 f = fract(st);
  st -= f;
  vec4 xc = cubic(f.x);
  vec4 yc = cubic(f.y);
  vec4 c = st.xxyy + vec2(-0.5, 1.5).xyxy;
  vec4 s = vec4(xc.xz + xc.yw, yc.xz + yc.yw);
  vec4 off = (c + vec4(xc.yw, yc.yw) / s) / u_sim.xxyy;
  float a = texture(u_field, off.xz).r;
  float b = texture(u_field, off.yz).r;
  float d = texture(u_field, off.xw).r;
  float e = texture(u_field, off.yw).r;
  float sx = s.x / (s.x + s.y);
  float sy = s.z / (s.z + s.w);
  return mix(mix(e, d, sx), mix(b, a, sx), sy);
}
void main() {
  float t = field(v_uv);
  vec2 q = vec2(v_uv.x * u_res.x / u_res.y, v_uv.y - u_offset);
  float drift = u_time * 0.014;
  float amb = 0.6 * noise(q * 1.7 + vec2(drift, -drift)) + 0.4 * noise(q * 4.1 - vec2(drift * 1.7, 0.0));
  float grain = hash(gl_FragCoord.xy * 0.731 + fract(u_time) * 61.0) - 0.5;
  float v = 0.05 + 0.25 * amb * amb * amb + t + grain * 0.03;
  vec3 col = texture(u_ramp, vec2(clamp(v, 0.0, 1.0), 0.5)).rgb;

  float b = t * 9.0;
  float d = abs(fract(b - 0.5) - 0.5) / max(fwidth(b), 1e-5);
  float iso = (1.0 - smoothstep(0.5, 1.5, d)) * smoothstep(0.07, 0.14, t) * (1.0 - smoothstep(0.86, 0.98, t));
  col = mix(col, vec3(1.0, 0.95, 0.86), iso * 0.2);
  o = vec4(col, 1.0);
}`;

export interface FrameInput {
  /** Seconds since the previous frame. */
  dt: number;
  /** Seconds since start; frozen when motion is reduced. */
  time: number;
  /** Pixels the content on screen moved up since the previous frame. */
  scrollDelta: number;
  /** Total of those moves: where the field is on the page. */
  scrollY: number;
  /** Which way heat rises on screen, unit length, y up. */
  up: readonly [number, number];
  /** 0 holds the field still (no rise, no swirl); 1 lets it move. */
  motion: number;
}

export class HeatField {
  readonly painter = new Painter();
  cssWidth = 1;
  cssHeight = 1;

  private readonly gl: WebGL2RenderingContext;
  private sim!: WebGLProgram;
  private show!: WebGLProgram;
  private uniforms = new Map<string, WebGLUniformLocation | null>();
  private targets: Array<{ tex: WebGLTexture; fbo: WebGLFramebuffer }> = [];
  private srcTex!: WebGLTexture;
  private rampTex!: WebGLTexture;
  private simW = 0;
  private simH = 0;
  private floatTargets = false;
  private pixelScale = 1;

  private constructor(
    private readonly canvas: HTMLCanvasElement,
    gl: WebGL2RenderingContext,
  ) {
    this.gl = gl;
    this.build();
    canvas.addEventListener("webglcontextlost", (e) => e.preventDefault());
    canvas.addEventListener("webglcontextrestored", () => {
      this.simW = 0;
      this.build();
      this.resize();
    });
  }

  static create(canvas: HTMLCanvasElement): HeatField | null {
    const gl = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      powerPreference: "high-performance",
    });
    if (!gl) return null;
    try {
      return new HeatField(canvas, gl);
    } catch (error) {
      console.warn("memoire: heat field disabled:", error);
      return null;
    }
  }

  /** Scale of the drawing buffer relative to CSS pixels; lowered when frames run long. */
  setPixelScale(scale: number): void {
    if (scale === this.pixelScale) return;
    this.pixelScale = scale;
    this.resize();
  }

  resize(): void {
    const { canvas } = this;
    this.cssWidth = Math.max(1, canvas.clientWidth);
    this.cssHeight = Math.max(1, canvas.clientHeight);
    const w = Math.round(this.cssWidth * this.pixelScale);
    const h = Math.round(this.cssHeight * this.pixelScale);
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
    }
    const aspect = this.cssWidth / this.cssHeight;
    const simW = Math.max(48, Math.round(Math.sqrt(46000 * aspect)));
    const simH = Math.max(48, Math.round(simW / aspect));
    if (simW !== this.simW || simH !== this.simH) {
      this.simW = simW;
      this.simH = simH;
      this.painter.resize(simW, simH);
      this.allocateTargets();
    }
  }

  /** First half of a frame: let the scenes draw this frame's heat sources. */
  paint(draw: (painter: Painter) => void): void {
    this.painter.begin(this.cssWidth);
    draw(this.painter);
  }

  /** Second half: step the field with the painted sources and show it. */
  render(input: FrameInput): void {
    const { gl } = this;
    if (gl.isContextLost()) return;

    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.srcTex);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, this.painter.canvas);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);

    const [prev, next] = this.targets as [(typeof this.targets)[0], (typeof this.targets)[0]];
    gl.bindFramebuffer(gl.FRAMEBUFFER, next.fbo);
    gl.viewport(0, 0, this.simW, this.simH);
    gl.useProgram(this.sim);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, prev.tex);
    gl.uniform1i(this.u("sim.u_prev"), 0);
    gl.uniform1i(this.u("sim.u_src"), 1);
    gl.uniform2f(this.u("sim.u_texel"), 1 / this.simW, 1 / this.simH);
    gl.uniform2f(this.u("sim.u_up"), input.up[0], input.up[1]);
    gl.uniform1f(this.u("sim.u_scroll"), input.scrollDelta / this.cssHeight);
    gl.uniform1f(this.u("sim.u_dt"), input.dt);
    gl.uniform1f(this.u("sim.u_time"), input.time);
    gl.uniform1f(this.u("sim.u_aspect"), this.cssWidth / this.cssHeight);
    gl.uniform1f(this.u("sim.u_motion"), input.motion);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    this.targets.reverse();

    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.useProgram(this.show);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, next.tex);
    gl.activeTexture(gl.TEXTURE2);
    gl.bindTexture(gl.TEXTURE_2D, this.rampTex);
    gl.uniform1i(this.u("show.u_field"), 0);
    gl.uniform1i(this.u("show.u_ramp"), 2);
    gl.uniform2f(this.u("show.u_sim"), this.simW, this.simH);
    gl.uniform2f(this.u("show.u_res"), this.cssWidth, this.cssHeight);
    gl.uniform1f(this.u("show.u_time"), input.time);
    gl.uniform1f(this.u("show.u_offset"), (input.scrollY / this.cssHeight) % 64);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  private u(key: string): WebGLUniformLocation | null {
    return this.uniforms.get(key) ?? null;
  }

  private build(): void {
    const { gl } = this;
    this.floatTargets =
      gl.getExtension("EXT_color_buffer_float") !== null ||
      gl.getExtension("EXT_color_buffer_half_float") !== null;

    this.sim = this.program(SIM_FRAG, "sim");
    this.show = this.program(SHOW_FRAG, "show");

    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

    this.srcTex = this.texture(gl.LINEAR);
    this.rampTex = this.texture(gl.LINEAR);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 256, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, rampPixels(256));
    gl.disable(gl.BLEND);
    gl.disable(gl.DEPTH_TEST);
  }

  private program(frag: string, name: string): WebGLProgram {
    const { gl } = this;
    const program = gl.createProgram();
    for (const [type, source] of [
      [gl.VERTEX_SHADER, VERT],
      [gl.FRAGMENT_SHADER, frag],
    ] as const) {
      const shader = gl.createShader(type);
      if (!shader) throw new Error("shader allocation failed");
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
        throw new Error(`${name} shader: ${gl.getShaderInfoLog(shader) ?? "compile failed"}`);
      }
      gl.attachShader(program, shader);
    }
    gl.bindAttribLocation(program, 0, "a_pos");
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      throw new Error(`${name} program: ${gl.getProgramInfoLog(program) ?? "link failed"}`);
    }
    const count = gl.getProgramParameter(program, gl.ACTIVE_UNIFORMS) as number;
    for (let i = 0; i < count; i++) {
      const info = gl.getActiveUniform(program, i);
      if (info) this.uniforms.set(`${name}.${info.name}`, gl.getUniformLocation(program, info.name));
    }
    return program;
  }

  private texture(filter: number): WebGLTexture {
    const { gl } = this;
    const tex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return tex;
  }

  private allocateTargets(): void {
    const { gl } = this;
    for (const t of this.targets) {
      gl.deleteTexture(t.tex);
      gl.deleteFramebuffer(t.fbo);
    }
    this.targets = [0, 1].map(() => {
      const tex = this.texture(gl.LINEAR);
      if (this.floatTargets) {
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA16F, this.simW, this.simH, 0, gl.RGBA, gl.HALF_FLOAT, null);
      } else {
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, this.simW, this.simH, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      }
      const fbo = gl.createFramebuffer();
      gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
      if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE) {
        throw new Error("heat target is not renderable");
      }
      gl.clearColor(0, 0, 0, 1);
      gl.clear(gl.COLOR_BUFFER_BIT);
      return { tex, fbo };
    });
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  }
}
