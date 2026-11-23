// Queqiao's icon: an Oriental magpie (喜鹊, Pica serica) perched on a small gold arch (the magpie bridge, 鹊桥 of the
// Qixi legend), drawn as SVG by this script (our own drawing; no outside images). assetsiconmake_icon.ps1 runs it and
// turns the SVGs into assetsqueqiao.ico.
// node magpie_icon.js <output folder>  ->  queqiao.svg (32 px and up), queqiao-small.svg (24 px and down: a stronger
// rim) and magpie-spread.svg (the magpie with its wings spread, for the main window's card)
const fs = require('fs');
const path = require('path');
const folder = process.argv[2] || __dirname;
const out = (name, text) => fs.writeFileSync(path.join(folder, name), text);

// ---------- geometry helpers ----------
const f = (n) => Number(n.toFixed(2));
// Closed or open smooth curve through points (Catmull-Rom -> cubic Bezier).
function smooth(points, closed = true, tension = 1) {
  const p = points;
  const n = p.length;
  const get = (i) => (closed ? p[(i + n) % n] : p[Math.max(0, Math.min(n - 1, i))]);
  let d = `M${f(p[0][0])} ${f(p[0][1])}`;
  const last = closed ? n : n - 1;
  for (let i = 0; i < last; i++) {
    const p0 = get(i - 1), p1 = get(i), p2 = get(i + 1), p3 = get(i + 2);
    const c1 = [p1[0] + ((p2[0] - p0[0]) / 6) * tension, p1[1] + ((p2[1] - p0[1]) / 6) * tension];
    const c2 = [p2[0] - ((p3[0] - p1[0]) / 6) * tension, p2[1] - ((p3[1] - p1[1]) / 6) * tension];
    d += ` C${f(c1[0])} ${f(c1[1])} ${f(c2[0])} ${f(c2[1])} ${f(p2[0])} ${f(p2[1])}`;
  }
  return d + (closed ? ' Z' : '');
}
const rad = (deg) => (deg * Math.PI) / 180;
// A feather: from `base` along `angle` (degrees, 0 = +x, y down), `len` long, `bend` degrees of curl over its length.
// Width grows from wBase to wMax at `at` (0..1) and ends in a rounded (round=1) or pointed (round=0) tip.
function feather({ base, angle, len, wBase = 2, wMax = 8, at = 0.7, bend = 0, round = 1, asym = 0 }) {
  const steps = 18;
  const centre = [];
  let x = base[0], y = base[1], a = rad(angle);
  const da = rad(bend) / steps;
  for (let i = 0; i <= steps; i++) {
    centre.push([x, y, a]);
    x += Math.cos(a) * (len / steps);
    y += Math.sin(a) * (len / steps);
    a += da;
  }
  const widthAt = (t) => {
    if (t <= at) return wBase + (wMax - wBase) * Math.sin((t / at) * Math.PI / 2);
    const u = (t - at) / (1 - at);
    return wMax * (round ? Math.sqrt(Math.max(0, 1 - u * u)) : 1 - u);
  };
  const left = [], right = [];
  centre.forEach(([cx, cy, ca], i) => {
    const t = i / steps;
    const w = widthAt(t) / 2;
    const nx = -Math.sin(ca), ny = Math.cos(ca);
    left.push([cx + nx * w * (1 + asym), cy + ny * w * (1 + asym)]);
    right.push([cx - nx * w * (1 - asym), cy - ny * w * (1 - asym)]);
  });
  const pts = left.concat(right.reverse());
  return { d: smooth(pts, true, 0.9), centre };
}
function shaft(fe, from = 0.08, to = 0.9) {
  const c = fe.centre;
  const a = Math.floor(from * (c.length - 1)), b = Math.floor(to * (c.length - 1));
  return smooth(c.slice(a, b + 1).map(([x, y]) => [x, y]), false);
}

// ---------- shared paint ----------
const defs = (id) => `
  <linearGradient id="${id}-black" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="#2a2f3e"/><stop offset=".45" stop-color="#0f121b"/><stop offset="1" stop-color="#05060a"/>
  </linearGradient>
  <radialGradient id="${id}-head" cx=".62" cy=".28" r=".75">
    <stop offset="0" stop-color="#4a5670"/><stop offset=".35" stop-color="#1a1f2c"/><stop offset="1" stop-color="#06070b"/>
  </radialGradient>
  <linearGradient id="${id}-wing" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="#3b4f9c"/><stop offset=".4" stop-color="#22306a"/><stop offset=".75" stop-color="#121733"/><stop offset="1" stop-color="#07080f"/>
  </linearGradient>
  <linearGradient id="${id}-tail" gradientUnits="userSpaceOnUse" x1="108" y1="147" x2="8" y2="244">
    <stop offset="0" stop-color="#0b0d14"/><stop offset=".25" stop-color="#173c40"/><stop offset=".55" stop-color="#2b6a55"/>
    <stop offset=".78" stop-color="#4b3e95"/><stop offset=".95" stop-color="#241a4a"/><stop offset="1" stop-color="#0d0b18"/>
  </linearGradient>
  <linearGradient id="${id}-white" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#ffffff"/><stop offset=".7" stop-color="#eef1f6"/><stop offset="1" stop-color="#c9cfdc"/>
  </linearGradient>
  <linearGradient id="${id}-gold" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#fbe4a3"/><stop offset="1" stop-color="#d99a3e"/>
  </linearGradient>
  <linearGradient id="${id}-beak" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#3a3f4c"/><stop offset=".5" stop-color="#14171f"/><stop offset="1" stop-color="#050608"/>
  </linearGradient>`;

// ---------- the perched magpie, facing right, in a 256 box (second pass) ----------
function perched(id) {
  const g = [];
  const T = (s) => g.push(s);
  // A small seeded random source, so the drawing is the same every time.
  let seed = 11;
  const rnd = () => { seed = (seed * 16807) % 2147483647; return (seed - 1) / 2147483646; };

  // Tail: graduated, the central feathers longest; drawn from the shortest (outer, lowest) to the longest (on top).
  const tb = [108, 147];
  const tail = [
    { dx: 0, dy: 0, angle: 136.5, len: 142, wMax: 18 },
    { dx: 1, dy: 2.6, angle: 137.6, len: 126, wMax: 17 },
    { dx: 2, dy: 5.2, angle: 138.7, len: 110, wMax: 16 },
    { dx: 3, dy: 7.8, angle: 139.8, len: 94, wMax: 15 },
    { dx: 4, dy: 10.4, angle: 140.9, len: 78, wMax: 14 },
  ];
  tail.forEach((t, i) => {
    const base = [tb[0] + t.dx, tb[1] + t.dy];
    const fe = feather({ base, angle: t.angle, len: t.len, wBase: 10, wMax: t.wMax, at: 0.82, bend: -2, round: 1 });
    T(`<path d="${fe.d}" fill="url(#${id}-tail)" stroke="#04050a" stroke-width="1.2"/>`);
    T(`<path d="${fe.d}" fill="none" stroke="#9ad9ff" stroke-opacity=".16" stroke-width=".7" transform="translate(-.4 -.6)"/>`);
    const sheen = feather({ base: [base[0] + 1.2, base[1] - 1], angle: t.angle - 0.6, len: t.len * 0.95, wBase: 2, wMax: t.wMax * 0.32, at: 0.8, bend: -2, round: 1 });
    T(`<path d="${sheen.d}" fill="#a6f3d6" opacity="${(0.06 + i * 0.025).toFixed(3)}"/>`);
    T(`<path d="${shaft(fe, 0.06, 0.95)}" fill="none" stroke="#c4f5e2" stroke-opacity="${(0.16 + i * 0.05).toFixed(2)}" stroke-width="0.9"/>`);
    // Barb lines towards the tip.
    for (let k = 0; k < 6; k++) {
      const c = fe.centre[Math.floor(10 + k * 1.2)];
      const a = c[2] + rad(150);
      T(`<path d="M${f(c[0])} ${f(c[1])} l${f(Math.cos(a) * t.wMax * 0.42)} ${f(Math.sin(a) * t.wMax * 0.42)}" stroke="#d8c8ff" stroke-opacity=".10" stroke-width=".8"/>`);
    }
  });

  // The bridge under its feet: a small gold arch.
  T(`<path d="M38 202 Q140 166 242 202" fill="none" stroke="url(#${id}-gold)" stroke-width="7.5" stroke-linecap="round"/>`);
  T(`<path d="M44 198 Q140 164 236 198" fill="none" stroke="#fff6d6" stroke-opacity=".55" stroke-width="1.6" stroke-linecap="round"/>`);
  // Legs and feet, gripping the arch.
  const leg = (x0, y0, x1, y1) => {
    let s = `<path d="M${x0} ${y0} L${x1} ${y1}" stroke="#0a0b10" stroke-width="4.6" stroke-linecap="round"/>`;
    s += `<path d="M${x0 - 0.6} ${y0 + 4} L${x1 - 0.6} ${y1 - 3}" stroke="#4a5162" stroke-opacity=".5" stroke-width="1"/>`;
    for (let k = 0; k < 4; k++) s += `<path d="M${f(x0 + (x1 - x0) * (0.25 + k * 0.18) - 2)} ${f(y0 + (y1 - y0) * (0.25 + k * 0.18))} h4" stroke="#2a2f3a" stroke-width=".8"/>`;
    s += `<path d="M${x1} ${y1} q7 -2 12 2.5 l2.2 2.6 M${x1} ${y1} q-7 -1 -11 3 l-1.6 2.8 M${x1} ${y1} q3 4 1.5 8" fill="none" stroke="#0a0b10" stroke-width="2.8" stroke-linecap="round" stroke-linejoin="round"/>`;
    return s;
  };
  T(leg(133, 158, 131, 181));
  T(leg(148, 156, 149, 180));

  // Body and head silhouette (black).
  const body = [
    [195, 66], [188, 54], [174, 48], [160, 53], [150, 65], [141, 82], [126, 99], [112, 117], [102, 135], [103, 149],
    [115, 158], [134, 163], [153, 159], [168, 147], [179, 129], [186, 109], [191, 92], [195, 79],
  ];
  T(`<path d="${smooth(body)}" fill="url(#${id}-black)"/>`);
  T(`<clipPath id="${id}-bodyclip"><path d="${smooth(body)}"/></clipPath><g clip-path="url(#${id}-bodyclip)">`);
  // Breast gloss and feather texture on the black.
  T(`<path d="M189 90 Q186 108 177 126" fill="none" stroke="#6a7bb0" stroke-opacity=".28" stroke-width="3.2" stroke-linecap="round"/>`);
  for (let k = 0; k < 46; k++) {
    const x = 150 + rnd() * 40, y = 60 + rnd() * 62;
    if (y > 118 && x < 165) continue;
    T(`<path d="M${f(x)} ${f(y)} q2.6 2.4 5.4 1.6" fill="none" stroke="#7d8cc0" stroke-opacity="${(0.08 + rnd() * 0.1).toFixed(2)}" stroke-width=".8"/>`);
  }
  T('</g>');
  // White belly and flank, with soft feather lines.
  const belly = [[180, 117], [174, 136], [162, 151], [145, 160], [126, 159], [117, 151], [126, 140], [145, 131], [163, 122], [173, 115]];
  T(`<path d="${smooth(belly)}" fill="url(#${id}-white)"/>`);
  for (let k = 0; k < 5; k++) {
    T(`<path d="M${128 + k * 6} ${155 - k * 2} q12 -5 ${18 + k} -${16 + k * 2}" fill="none" stroke="#aab3c8" stroke-opacity="${(0.55 - k * 0.07).toFixed(2)}" stroke-width=".9"/>`);
  }

  // The folded wing, back to front: primaries (white inner webs), secondaries, coverts, then the white scapulars.
  const prim = [
    { base: [146, 110], angle: 147, len: 74, wMax: 12 },
    { base: [143, 114], angle: 147.5, len: 70, wMax: 12 },
    { base: [140, 118], angle: 148, len: 66, wMax: 12 },
    { base: [137, 122], angle: 148.5, len: 61, wMax: 12 },
  ];
  const primShapes = prim.map((p) => feather({ ...p, wBase: 7, at: 0.6, bend: 3, round: 1 }));
  primShapes.forEach((fe) => T(`<path d="${fe.d}" fill="#0a0c15" stroke="#04050a" stroke-width=".8"/>`));
  prim.forEach((p, i) => {
    const inner = feather({ ...p, base: [p.base[0] + 0.6, p.base[1] - 3.4], len: p.len * 0.86, wBase: 1.4, wMax: 3.6, at: 0.6, bend: 3, round: 1 });
    T(`<path d="${inner.d}" fill="url(#${id}-white)" opacity="${(0.95 - i * 0.08).toFixed(2)}"/>`);
    T(`<path d="${shaft(primShapes[i], 0.1, 0.9)}" fill="none" stroke="#5c6890" stroke-opacity=".45" stroke-width=".8"/>`);
  });
  const sec = [
    { base: [152, 104], angle: 141, len: 48 }, { base: [148, 108], angle: 142, len: 47 },
    { base: [144, 112], angle: 143, len: 45 }, { base: [140, 116], angle: 144, len: 42 }, { base: [136, 120], angle: 145, len: 38 },
  ];
  sec.forEach((s) => {
    const fe = feather({ ...s, wBase: 9, wMax: 13, at: 0.6, bend: 4, round: 1 });
    T(`<path d="${fe.d}" fill="url(#${id}-wing)" stroke="#05060c" stroke-width=".9"/>`);
    T(`<path d="${fe.d}" fill="none" stroke="#8ea3ff" stroke-opacity=".22" stroke-width=".7"/>`);
  });
  // Coverts: rows of small rounded feathers over the front of the wing.
  const rows = [
    { from: [160, 92], to: [136, 116], n: 5, len: 16, w: 10, angle: 128 },
    { from: [156, 100], to: [134, 122], n: 5, len: 15, w: 10, angle: 132 },
    { from: [152, 107], to: [133, 126], n: 4, len: 14, w: 9, angle: 136 },
  ];
  rows.forEach((r, ri) => {
    for (let k = 0; k < r.n; k++) {
      const t = k / (r.n - 1);
      const base = [r.from[0] + (r.to[0] - r.from[0]) * t, r.from[1] + (r.to[1] - r.from[1]) * t];
      const fe = feather({ base, angle: r.angle, len: r.len, wBase: r.w * 0.7, wMax: r.w, at: 0.5, bend: 6, round: 1 });
      T(`<path d="${fe.d}" fill="url(#${id}-wing)" stroke="#05060c" stroke-width=".7"/>`);
      T(`<path d="${fe.d}" fill="none" stroke="#a9b8ff" stroke-opacity="${(0.28 - ri * 0.05).toFixed(2)}" stroke-width=".6"/>`);
    }
  });
  // White scapular patch, a large oval on the shoulder.
  const scap = [[163, 79], [154, 84], [141, 93], [128, 104], [119, 113], [122, 117], [135, 111], [149, 102], [160, 94], [167, 86]];
  T(`<path d="${smooth(scap)}" fill="url(#${id}-white)"/>`);
  for (let k = 0; k < 4; k++) {
    T(`<path d="M${160 - k * 9} ${88 + k * 6.5} q-5 4 -9 9" fill="none" stroke="#aeb7cb" stroke-opacity=".7" stroke-width=".8"/>`);
  }

  // Head: gloss, eye, beak with the bristles over the nostrils.
  T(`<ellipse cx="173" cy="66" rx="21" ry="18.5" fill="url(#${id}-head)"/>`);
  T(`<path d="M160 54 Q174 46 189 54" fill="none" stroke="#9fb0ea" stroke-opacity=".38" stroke-width="2.4" stroke-linecap="round"/>`);
  T(`<path d="M156 70 Q160 80 168 84" fill="none" stroke="#7182c0" stroke-opacity=".22" stroke-width="2" stroke-linecap="round"/>`);
  T(`<path d="M191 60 Q209 61 227 78 Q211 75.6 193 76 Z" fill="url(#${id}-beak)"/>`);
  T(`<path d="M193 76 Q209 76.8 224 80 Q209 84.5 194 83 Z" fill="#0a0b10"/>`);
  T(`<path d="M193.5 76.1 Q209 77 224 79.7" fill="none" stroke="#000" stroke-width="1.2"/>`);
  T(`<path d="M195 63.5 Q208 65 219 73" fill="none" stroke="#959eb4" stroke-opacity=".55" stroke-width="1.1"/>`);
  T(`<path d="M189 60 q5 -1.6 10 0.6 M189 63 q5 -0.8 9 1.6" fill="none" stroke="#2c3242" stroke-width="1.1" stroke-linecap="round"/>`);
  T(`<circle cx="183.5" cy="63" r="4.4" fill="#120c08"/><circle cx="183.5" cy="63" r="3" fill="#3b2617"/><circle cx="183.5" cy="63" r="1.6" fill="#07050a"/><circle cx="184.8" cy="61.6" r="1.1" fill="#fff"/>`);
  T(`<circle cx="183.5" cy="63" r="5.4" fill="none" stroke="#3a4152" stroke-opacity=".6" stroke-width=".8"/>`);
  return g.join('\n');
}



// ---------- the magpie with its wings spread (the bridge's magpies; the main window's card) ----------
// One wing (the bird's left, on the right of the picture); the other is its mirror image.
// lift: degrees the whole wing is raised (positive = up), for variety along the bridge.
function wingHalf(id, lift) {
  const s = [];
  const L = rad(-lift);
  // Rotate a point about the shoulder by the lift.
  const S = [133, 84];
  const rot = ([x, y]) => [S[0] + (x - S[0]) * Math.cos(L) - (y - S[1]) * Math.sin(L), S[1] + (x - S[0]) * Math.sin(L) + (y - S[1]) * Math.cos(L)];
  const W = rot([180, 74]);
  const ang = (a) => a - lift;
  // Secondaries: along the arm, pointing down and back; black with a faint blue gloss.
  const nSec = 8;
  for (let k = nSec - 1; k >= 0; k--) {
    const t = k / (nSec - 1);
    const base = [S[0] + (W[0] - S[0]) * t, S[1] + (W[1] - S[1]) * t + 4];
    const fe = feather({ base, angle: ang(96 - t * 22), len: 30 - t * 3, wBase: 8, wMax: 10.5, at: 0.6, bend: -4, round: 1 });
    s.push(`<path d="${fe.d}" fill="#141826" stroke="#04050a" stroke-width=".8"/>`);
    s.push(`<path d="${shaft(fe, 0.1, 0.9)}" fill="none" stroke="#6f7fb8" stroke-opacity=".35" stroke-width=".6"/>`);
    s.push(`<path d="${fe.d}" fill="none" stroke="#8fa0e0" stroke-opacity=".18" stroke-width=".6"/>`);
  }
  // Primaries: from the hand, fanning from along the wing (outermost) to pointing down (innermost).
  // From below, white with black outer edges and black tips: the magpie's white wing panel.
  const nP = 9;
  const prim = [];
  for (let k = 0; k < nP; k++) {
    const t = k / (nP - 1); // 0 = innermost
    const base = [W[0] + 9 * t, W[1] + 2 - 3 * t];
    const a = ang(70 - t * 72);
    const len = 34 + 14 * Math.sin(t * Math.PI * 0.85);
    prim.push({ base, a, len });
  }
  prim.forEach(({ base, a, len }) => {
    const fe = feather({ base, angle: a, len, wBase: 6, wMax: 10, at: 0.62, bend: -6, round: 1 });
    s.push(`<path d="${fe.d}" fill="#0b0d16" stroke="#04050a" stroke-width=".8"/>`);
    const white = feather({ base: [base[0] + Math.cos(rad(a + 90)) * 1.4, base[1] + Math.sin(rad(a + 90)) * 1.4], angle: a + 1.5, len: len * 0.72, wBase: 4, wMax: 6.6, at: 0.62, bend: -6, round: 1 });
    s.push(`<path d="${white.d}" fill="url(#${id}-white)"/>`);
    s.push(`<path d="${shaft(fe, 0.08, 0.92)}" fill="none" stroke="#3a4152" stroke-opacity=".6" stroke-width=".6"/>`);
  });
  // Underwing coverts: the leading edge, black with gloss, in small rounded rows.
  const lead = [S, [S[0] + (W[0] - S[0]) * 0.35, S[1] + (W[1] - S[1]) * 0.35 - 3], [S[0] + (W[0] - S[0]) * 0.7, S[1] + (W[1] - S[1]) * 0.7 - 3], [W[0] + 6, W[1] - 1],
    [W[0] + 2, W[1] + 9], [S[0] + (W[0] - S[0]) * 0.55, S[1] + (W[1] - S[1]) * 0.55 + 11], [S[0] - 2, S[1] + 12]];
  s.push(`<path d="${smooth(lead)}" fill="url(#${id}-wing)" stroke="#04050a" stroke-width=".7"/>`);
  for (let r = 0; r < 2; r++) {
    for (let k = 0; k < 6; k++) {
      const t = 0.08 + k * 0.17;
      const x = S[0] + (W[0] - S[0]) * t, y = S[1] + (W[1] - S[1]) * t + 3 + r * 5;
      s.push(`<path d="M${f(x - 4)} ${f(y)} q4 4.5 8.5 0" fill="none" stroke="#a9b8ff" stroke-opacity="${(0.3 - r * 0.1).toFixed(2)}" stroke-width=".7"/>`);
    }
  }
  return s.join('\n');
}

function spread(id, lift = 8) {
  const g = [];
  const T = (s) => g.push(s);
  // Tail first (behind the body): graduated, a little fanned.
  [[90, 74, 15], [95, 66, 14], [85, 66, 14], [100, 56, 13], [80, 56, 13]].reverse().forEach(([a, len, w], i) => {
    const fe = feather({ base: [120, 118], angle: a, len, wBase: 10, wMax: w, at: 0.84, bend: 0, round: 1 });
    T(`<path d="${fe.d}" fill="url(#${id}-vtail)" stroke="#04050a" stroke-width="1"/>`);
    T(`<path d="${shaft(fe, 0.06, 0.95)}" fill="none" stroke="#c4f5e2" stroke-opacity=".25" stroke-width=".7"/>`);
  });
  // Wings: the right of the picture, then its mirror.
  const half = wingHalf(id, lift);
  T(half);
  T(`<g transform="translate(240 0) scale(-1 1)">${half}</g>`);
  // Body: black, with the white belly; black vent; feet tucked.
  T(`<path d="${smooth([[120, 64], [131, 72], [136, 90], [133, 110], [127, 124], [120, 128], [113, 124], [107, 110], [104, 90], [109, 72]])}" fill="url(#${id}-black)"/>`);
  T(`<path d="${smooth([[120, 88], [129, 94], [130, 106], [125, 117], [120, 119], [115, 117], [110, 106], [111, 94]])}" fill="url(#${id}-white)"/>`);
  T(`<path d="M116 100 q4 3 8 0 M115 107 q5 3 10 0" fill="none" stroke="#aab3c8" stroke-width=".8" stroke-opacity=".7"/>`);
  T(`<path d="M116 122 l-2 7 M124 122 l2 7" stroke="#0a0b10" stroke-width="2.2" stroke-linecap="round"/>`);
  // Head, seen from below: beak pointing up, the eyes at the sides.
  T(`<ellipse cx="120" cy="60" rx="12.5" ry="13.5" fill="url(#${id}-head)"/>`);
  T(`<path d="M113 52 Q120 46 127 52" fill="none" stroke="#9fb0ea" stroke-opacity=".35" stroke-width="1.6" stroke-linecap="round"/>`);
  T(`<path d="M115.2 51 Q120 20 124.8 51 Z" fill="url(#${id}-beak)"/>`);
  T(`<path d="M120 38 L120 49.5" stroke="#000" stroke-width=".8"/>`);
  T(`<circle cx="110.5" cy="58" r="2.2" fill="#120c08"/><circle cx="129.5" cy="58" r="2.2" fill="#120c08"/>`);
  T(`<circle cx="110" cy="57.4" r=".55" fill="#fff"/><circle cx="130" cy="57.4" r=".55" fill="#fff"/>`);
  return g.join('\n');
}

const vdefs = (id) => defs(id) + `
  <linearGradient id="${id}-vtail" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#0b0d14"/><stop offset=".3" stop-color="#173c40"/><stop offset=".62" stop-color="#2b6a55"/>
    <stop offset=".86" stop-color="#4b3e95"/><stop offset="1" stop-color="#1c143c"/>
  </linearGradient>`;


// A light rim around the bird, so that it shows on a dark taskbar too.
const rim = (id, radius, opacity) => `
  <filter id="${id}-rim" x="-10%" y="-10%" width="120%" height="120%">
    <feMorphology in="SourceAlpha" operator="dilate" radius="${radius}" result="d"/>
    <feFlood flood-color="#f4f6fc" flood-opacity="${opacity}"/>
    <feComposite in2="d" operator="in" result="o"/>
    <feMerge><feMergeNode in="o"/><feMergeNode in="SourceGraphic"/></feMerge>
  </filter>`;
const icon = (id, radius, opacity) =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256"><defs>${defs(id)}${rim(id, radius, opacity)}</defs><g filter="url(#${id}-rim)">${perched(id)}</g></svg>`;
out('queqiao.svg', icon('q', 3.2, 0.92));
out('queqiao-small.svg', icon('s', 11, 1));
// The spread-winged magpie, centred in a square, with a soft rim (it sits on the night-blue card).
out(
  'magpie-spread.svg',
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="-8 -22 256 256"><defs>${vdefs('m')}${rim('m', 4, 0.55)}</defs><g filter="url(#m-rim)">${spread('m', 8)}</g></svg>`
);
console.log('icon svgs written to ' + folder);
