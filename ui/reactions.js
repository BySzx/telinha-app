import { REACTION_IMAGES } from './reaction-images.js';
import { STICKERS } from './stickers.js';

const EMOJIS = ['👍', '❤️', '😂', '😮', '😢', '🔥', '👏', '🎉', '💀', '🤡'];
const SEND_INTERVAL_MS = 250;
const MAX_ON_SCREEN = 40;
const IMAGES = { ...REACTION_IMAGES, ...STICKERS };

let layer = null;
let lastSent = 0;

export const isValidReaction = (key) =>
  typeof key === 'string' && (EMOJIS.includes(key) || Object.hasOwn(IMAGES, key));

export function initReactions(onSend) {
  const button = document.getElementById('reactBtn');
  const picker = document.getElementById('reactPicker');

  const pick = (key) => {
    const now = Date.now();
    if (now - lastSent < SEND_INTERVAL_MS) return;
    lastSent = now;
    onSend(key);
    showReaction('Você', key);
  };

  for (const emoji of EMOJIS) {
    picker.append(pickerButton(emoji, emoji, () => pick(emoji)));
  }
  for (const [key, { label, src }] of Object.entries(IMAGES)) {
    const img = document.createElement('img');
    img.src = src;
    img.alt = label;
    picker.append(pickerButton(img, label, () => pick(key)));
  }

  button.addEventListener('click', (ev) => {
    ev.stopPropagation();
    picker.hidden = !picker.hidden;
    button.setAttribute('aria-expanded', String(!picker.hidden));
  });
  const close = () => {
    picker.hidden = true;
    button.setAttribute('aria-expanded', 'false');
  };
  document.addEventListener('click', (ev) => {
    if (!picker.hidden && !picker.contains(ev.target)) close();
  });
  document.addEventListener('keydown', (ev) => {
    if (ev.key === 'Escape') close();
  });
  document.addEventListener('fullscreenchange', () => {
    if (layer) (document.fullscreenElement ?? document.body).append(layer);
  });
}

export function showReaction(name, key) {
  if (!isValidReaction(key)) return;

  const host = document.fullscreenElement ?? document.body;
  if (!layer) {
    layer = document.createElement('div');
    layer.className = 'reaction-layer';
  }
  if (layer.parentElement !== host) host.append(layer);
  while (layer.childElementCount >= MAX_ON_SCREEN) layer.firstElementChild?.remove();

  const item = document.createElement('div');
  item.className = 'reaction';
  item.style.left = `${2 + Math.random() * 86}%`;
  item.style.bottom = `${Math.random() * 60}%`;
  item.style.setProperty('--sway', `${Math.round((Math.random() * 2 - 1) * 50)}px`);
  item.style.animationDuration = `${3.2 + Math.random() * 1.2}s`;

  const image = IMAGES[key];
  let visual;
  if (image) {
    visual = document.createElement('img');
    visual.src = image.src;
    visual.alt = image.label;
  } else {
    visual = document.createElement('span');
    visual.className = 'reaction-emoji';
    visual.textContent = key;
  }

  const label = document.createElement('span');
  label.className = 'reaction-name';
  label.textContent = name;

  item.append(visual, label);
  item.addEventListener('animationend', () => item.remove(), { once: true });
  layer.append(item);
}

function pickerButton(content, title, onClick) {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.title = title;
  btn.append(content);
  btn.addEventListener('click', onClick);
  return btn;
}
