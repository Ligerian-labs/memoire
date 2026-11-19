const commands = [
  ["memoire db create scratch --burstable", "scratch created\nSQLite · burstable · 24h idle TTL"],
  ["memoire db create notes --persistent", "notes created\nSQLite · persistent · snapshots every 6h"],
  ['memoire query notes "SELECT 42 AS answer" --json', '[{"answer":42}]'],
];

const motion = matchMedia("(prefers-reduced-motion: reduce)");
const command = document.querySelector("#command");
const result = document.querySelector("#result");
const copy = document.querySelector("#copy");
const buttons = document.querySelectorAll("[data-step]");
let copyTimer;

for (const button of buttons) {
  button.addEventListener("click", () => {
    const [input, output] = commands[Number(button.dataset.step)];
    command.textContent = input;
    result.textContent = output;
    copy.textContent = "Copy";
    for (const option of buttons) option.setAttribute("aria-pressed", String(option === button));
    if (!motion.matches) result.animate([{ opacity: 0.2, translate: "0 8px" }, { opacity: 1, translate: "0 0" }], 280);
  });
}

if (navigator.clipboard) {
  copy.hidden = false;
  copy.addEventListener("click", async () => {
    const input = command.textContent;
    clearTimeout(copyTimer);
    try {
      await navigator.clipboard.writeText(input);
      copy.textContent = command.textContent === input ? "Copied" : "Copy";
    } catch {
      copy.textContent = "Retry";
    }
    copyTimer = setTimeout(() => { copy.textContent = "Copy"; }, 1600);
  });
}

const canvas = document.querySelector("#field");
const context = canvas.getContext("2d");
if (context) {
  const hero = canvas.closest(".hero");
  const points = Array.from({ length: 720 }, (_, index) => {
    const vertical = 1 - 2 * (index + 0.5) / 720;
    const radius = Math.sqrt(1 - vertical * vertical);
    const angle = index * Math.PI * (3 - Math.sqrt(5));
    return [radius * Math.cos(angle), vertical, radius * Math.sin(angle)];
  });
  let frame = 0;
  let visible = true;
  let pointer = 0;
  let width = 0;
  let height = 0;

  function render(time) {
    frame = 0;
    const phase = motion.matches ? 0.7 : time * 0.0003 + pointer * 0.32 + scrollY * 0.0015;
    const sine = Math.sin(phase);
    const cosine = Math.cos(phase);
    const radius = Math.min(width * 0.39, height * 0.42);
    const centerX = width * 0.62;
    const centerY = height * 0.51;
    context.clearRect(0, 0, width, height);

    for (const [x, y, z] of points) {
      const depth = x * sine + z * cosine;
      const scale = 1 / (1.5 - depth * 0.42);
      const deformation = 1 + Math.sin(y * 12 + phase * 2) * 0.12;
      const px = centerX + (x * cosine - z * sine) * radius * scale * deformation;
      const py = centerY + y * radius * scale * 1.2;
      context.fillStyle = `rgba(220,255,119,${(depth + 1.13) * 0.4})`;
      const size = 1 + scale * 1.1;
      context.fillRect(px, py, size, size);
    }

    context.save();
    context.translate(centerX, centerY);
    context.rotate(-0.28);
    context.strokeStyle = "#dcff7740";
    for (let ring = 1; ring <= 3; ring++) {
      context.beginPath();
      context.ellipse(0, 0, radius * (0.98 + ring * 0.21), radius * (0.28 + ring * 0.1), 0, 0, Math.PI * 2);
      context.stroke();
    }
    context.restore();
    schedule();
  }

  function schedule() {
    if (!frame && visible && !document.hidden && !motion.matches) frame = requestAnimationFrame(render);
  }

  new ResizeObserver(() => {
    const ratio = Math.min(devicePixelRatio, 2);
    width = canvas.clientWidth;
    height = canvas.clientHeight;
    canvas.width = width * ratio;
    canvas.height = height * ratio;
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    if (motion.matches) render(0);
    else schedule();
  }).observe(canvas);
  new IntersectionObserver(([entry]) => {
    visible = entry.isIntersecting;
    schedule();
  }).observe(hero);
  document.addEventListener("visibilitychange", schedule);
  motion.addEventListener("change", () => {
    cancelAnimationFrame(frame);
    render(performance.now());
  });
  hero.addEventListener("pointermove", (event) => {
    if (event.pointerType === "mouse") pointer = event.clientX / innerWidth - 0.5;
  });
}
