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

// Parallel slices of the same folded M used in the wordmark.
const contours = [
  [[-1,.65],[-.77,-.44],[-.44,-.44],[-.22,-.02],[.25,-.44],[.57,-.44],[.83,.65],[.39,.65],[.26,.11],[-.24,.56],[-.48,.08],[-.61,.65]],
  [[-.37,-.62],[-.13,-.97],[.28,-.97],[-.08,-.62]],
];
const canvas = document.querySelector("#field");
const ctx = canvas.getContext("2d");
const slider = document.querySelector("#spread");
if (ctx) {
  let frame = 0;
  let elapsed = 0;
  let lastTime = 0;
  let visible = true;
  let width = 0;
  let height = 0;
  let tilt = 0;

  function draw(time) {
    frame = 0;
    elapsed += lastTime ? Math.min(time - lastTime, 45) : 0;
    lastTime = time;
    const wave = motion.matches ? 0 : Math.sin(elapsed / 2100);
    const separation = Number(slider.value) / 100;
    const scale = Math.min(width / 3.05, height / 2.55) / (1 + separation * .28);
    if (!scale) return;
    ctx.clearRect(0, 0, width, height);
    ctx.save();
    ctx.translate(width * .5, height * .52);
    ctx.scale(scale, scale);
    const shadow = ctx.createRadialGradient(0,0,.03,0,0,1);
    shadow.addColorStop(0,"#692d3566");
    shadow.addColorStop(1,"#80515200");
    ctx.save();
    ctx.translate(.02,.9);
    ctx.scale(1.15,.12);
    ctx.fillStyle = shadow;
    ctx.fillRect(-1,-1,2,2);
    ctx.restore();
    const entrance = motion.matches ? 0 : Math.max(0, 1 - elapsed / 1250) ** 2;
    const depth = .45 + separation * .65 + wave * .1 + entrance * .55;
    const angle = -.19 + wave * .055 + tilt * .045;
    ctx.rotate(angle);
    for (let i = 0; i < 27; i++) {
      const z = i / 26 - .5;
      ctx.save();
      ctx.rotate(z * (separation * .96 + entrance * .58 + wave * .14));
      ctx.transform(.91,-.14,.08,.95,z * depth,-z * depth * .52);
      for (const contour of contours) {
        ctx.beginPath();
        contour.forEach(([x,y], j) => j ? ctx.lineTo(x,y) : ctx.moveTo(x,y));
        ctx.closePath();
        const ink = ctx.createLinearGradient(-1,-.9,.85,.65);
        if (i === 26) {
          ink.addColorStop(0,"#ff754c"); ink.addColorStop(.5,"#e84220"); ink.addColorStop(1,"#ae281a");
        } else {
          ink.addColorStop(0,"#95302e"); ink.addColorStop(.55,"#ed5537"); ink.addColorStop(1,"#772924");
        }
        ctx.fillStyle = ink; ctx.fill();
        ctx.strokeStyle = i === 26 ? "#ff9d72" : "#60201c";
        ctx.lineWidth = .009; ctx.stroke();
      }
      ctx.restore();
    }
    ctx.restore();
    if (visible && !document.hidden && !motion.matches) frame = requestAnimationFrame(draw);
  }
  function refresh() {
    cancelAnimationFrame(frame);
    lastTime = 0;
    frame = requestAnimationFrame(draw);
  }
  new ResizeObserver(() => {
    width = canvas.clientWidth; height = canvas.clientHeight;
    const ratio = Math.min(devicePixelRatio, 2);
    canvas.width = width * ratio; canvas.height = height * ratio;
    ctx.setTransform(ratio,0,0,ratio,0,0);
    refresh();
  }).observe(canvas);
  new IntersectionObserver(([entry]) => {visible = entry.isIntersecting;refresh()}).observe(canvas);
  canvas.addEventListener("pointermove", event => {tilt = event.offsetX / width - .5});
  slider.addEventListener("input",refresh);
  document.addEventListener("visibilitychange",refresh);
  motion.addEventListener("change",refresh);
}
