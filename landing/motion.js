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

// The small wordmark, sculpture and closing impression use one set of outlines.
const plates = Array.from(document.querySelectorAll("#ligature path"), path => new Path2D(path.getAttribute("d")));
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
    const travel = motion.matches ? 0 : Math.min(1, scrollY / Math.max(1, canvas.clientHeight));
    const separation = Number(slider.value) / 100 + travel * .78;
    const portrait = width < 740;
    const scale = Math.min(width / (portrait ? 1.42 : 2.2), height / 1.78) / (1 + separation * .16);
    if (!scale) return;
    ctx.clearRect(0, 0, width, height);
    ctx.save();
    ctx.translate(width * ((portrait ? .74 : .68) - travel * .17), height * ((portrait ? .52 : .54) - travel * .09));
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
    ctx.rotate(angle + travel * .32);
    for (let i = 0; i < 27; i++) {
      const z = i / 26 - .5;
      ctx.save();
      ctx.rotate(z * (separation * .96 + entrance * .58 + wave * .14));
      ctx.transform(.91,-.14,.08,.95,z * depth,-z * depth * .52);
      ctx.scale(.018,.018);
      ctx.translate(-59,-28);
      const ink = ctx.createLinearGradient(0,-8,108,61);
      if (i === 26) {
        ink.addColorStop(0,"#ff7949");
        ink.addColorStop(.5,"#e6381b");
        ink.addColorStop(1,"#a42116");
      } else {
        ink.addColorStop(0,"#852222");
        ink.addColorStop(.55,"#ed5537");
        ink.addColorStop(1,"#681b18");
      }
      ctx.fillStyle = ink;
      ctx.strokeStyle = i === 26 ? "#ff9d72" : "#60201c";
      ctx.lineWidth = .36;
      for (const plate of plates) {
        ctx.fill(plate, "evenodd");
        ctx.stroke(plate);
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
  // Stop painting when the outgoing hero clears the header.
  const hero = canvas.closest("section");
  function checkVisibility() {
    const next = hero.getBoundingClientRect().bottom > header.offsetHeight;
    if (visible !== next) {
      visible = next;
      refresh();
    }
  }
  window.addEventListener("scroll", checkVisibility, {passive:true});
  canvas.addEventListener("pointermove", event => {tilt = event.offsetX / width - .5});
  slider.addEventListener("input",refresh);
  document.addEventListener("visibilitychange",refresh);
  motion.addEventListener("change",refresh);
}

const deckCards = [...document.querySelectorAll("main > section")];
const navigation = [...document.querySelectorAll('nav a[href^="#"]')];
const sections = navigation.map(link => document.querySelector(link.hash).nextElementSibling);
const header = document.querySelector(".masthead");
const menuToggle = header.querySelector(".menu-toggle");
const menuLabel = menuToggle.querySelector(".menu-label");
const menu = document.querySelector("#main-nav");
const compactMenu = matchMedia("(max-width:720px)");

function setMenu(open) {
  menuToggle.setAttribute("aria-expanded", String(open));
  menuLabel.textContent = open ? "Close" : "Menu";
  menuToggle.hidden = !compactMenu.matches;
  menu.hidden = compactMenu.matches && !open;
}
menuToggle.addEventListener("click", () => setMenu(menu.hidden));
menu.addEventListener("click", event => {
  const link = event.target.closest("a");
  if (!link || !compactMenu.matches) return;
  setMenu(false);
  if (link.hash) {
    const section = document.querySelector(link.hash).nextElementSibling;
    section.tabIndex = -1;
    section.focus({preventScroll:true});
  }
});
document.addEventListener("pointerdown", event => {
  if (!header.contains(event.target) && !menu.hidden && compactMenu.matches) setMenu(false);
});
header.addEventListener("focusout", event => {
  if (!header.contains(event.relatedTarget) && compactMenu.matches) setMenu(false);
});
document.addEventListener("keydown", event => {
  if (event.key === "Escape" && compactMenu.matches && !menu.hidden) {
    setMenu(false);
    menuToggle.focus();
  }
});
header.querySelector(".brand").addEventListener("click", () => setMenu(false));
compactMenu.addEventListener("change", () => setMenu(false));
window.addEventListener("pageshow", () => setMenu(false));
setMenu(false);

let scrollFrame = 0;
function updateNavigation() {
  scrollFrame = 0;
  const end = document.documentElement.scrollHeight - innerHeight;
  header.style.setProperty("--read", end > 0 ? Math.min(1, Math.max(0, scrollY / end)) : 1);
  const marker = innerHeight * .45;
  const foremost = deckCards.find(section => {
    const bounds = section.getBoundingClientRect();
    return bounds.top <= marker && marker < bounds.bottom;
  });
  sections.forEach((section, index) => {
    if (section === foremost) {
      navigation[index].setAttribute("aria-current", "location");
    } else {
      navigation[index].removeAttribute("aria-current");
    }
  });
}
function queueNavigation() {
  if (!scrollFrame) scrollFrame = requestAnimationFrame(updateNavigation);
}
window.addEventListener("scroll", queueNavigation, {passive:true});
window.addEventListener("resize", queueNavigation);
updateNavigation();


// Align the waiting card below the header even when its content exceeds one screen.
const cardSizes = new ResizeObserver(entries => {
  for (const {target} of entries) {
    target.style.setProperty("--card-height", target.offsetHeight + "px");
  }
});
for (const card of deckCards) cardSizes.observe(card);
