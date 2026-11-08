// One Capishot tab at a time (tab_gate.rs). Each tab is a whole copy of the app: its own graphics
// device, its own memory for every open page, its own worker pool. Two of them on a phone or a
// small graphics card are what makes the browser drop one. The tab that runs holds a Web Lock;
// another one that opens shows "Use here" over the page instead of starting. Choosing it asks the
// running tab to checkpoint its documents and step aside (it reloads paused, with its own "Use
// here"); a tab that doesn't answer (frozen) has the lock taken from it. The session comes back in
// the new tab from the checkpoints (recovery.rs).

const LOCK = 'capishot-tab';
const CHANNEL = 'capishot-tab';
const PAUSED = 'capishot.tabPaused';
// The running tab says it is saving at least this often while it hands over; silent longer, it is
// taken for frozen and the lock is taken.
const QUIET_MS = 4000;

let takeover = false;
let channel = null;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const TEXTS = {
  es: { other: 'Capishot está abierto en otra pestaña.', paused: 'Capishot se abrió en otra pestaña.', use: 'Usar aquí', note: 'La otra pestaña guarda tu trabajo y se pausa. Tus documentos se abren aquí como estaban.', wait: 'Guardando en la otra pestaña…' },
  pt: { other: 'O Capishot está aberto em outra aba.', paused: 'O Capishot foi aberto em outra aba.', use: 'Usar aqui', note: 'A outra aba salva o seu trabalho e fica em pausa. Seus documentos abrem aqui como estavam.', wait: 'Salvando na outra aba…' },
  en: { other: 'Capishot is open in another tab.', paused: 'Capishot was opened in another tab.', use: 'Use here', note: 'The other tab saves your work and pauses. Your documents open here as they were.', wait: 'Saving in the other tab…' },
};
const texts = () => {
  for (const l of navigator.languages || [navigator.language || 'en']) {
    const k = String(l).slice(0, 2).toLowerCase();
    if (TEXTS[k]) return TEXTS[k];
  }
  return TEXTS.en;
};

// Holds the lock while the page lives. Resolves true once held, false when `ifAvailable` finds it
// taken. A lock taken from this page (another tab chose "Use here" while this one didn't answer)
// pauses it.
function hold(options) {
  return new Promise((resolve) => {
    navigator.locks
      .request(LOCK, options, (lock) => {
        if (!lock) return resolve(false);
        resolve(true);
        return new Promise(() => {});
      })
      .catch((e) => {
        if (e && e.name === 'AbortError') pause();
        resolve(false);
      });
  });
}

function listen() {
  channel = new BroadcastChannel(CHANNEL);
  channel.onmessage = (e) => {
    if (e.data === 'takeover') {
      takeover = true;
      channel.postMessage('saving');
    }
  };
}

function pause() {
  try { sessionStorage.setItem(PAUSED, '1'); } catch (e) { /* storage denied: it starts asking again */ }
  location.reload();
}

// The overlay with "Use here"; resolves when it is chosen.
function ask(paused) {
  const t = texts();
  const el = document.createElement('div');
  el.id = 'capishot-tab-gate';
  el.setAttribute('style', 'position:fixed;inset:0;z-index:2147483647;background:#1b1e24;overflow:auto');
  el.innerHTML =
    '<div style="max-width:420px;margin:18vh auto;padding:24px;border-radius:12px;background:#23272f;color:#e6e9ef;' +
    'font:15px/1.45 system-ui,sans-serif;box-shadow:0 8px 40px #000a;text-align:center">' +
    '<h2 style="margin:0 0 10px;font-size:18px"></h2><p style="margin:0 0 16px;color:#b8c0cc"></p>' +
    '<button style="padding:12px 28px;font:inherit;font-weight:600;border-radius:8px;border:0;background:#3b6fd8;color:#fff;cursor:pointer"></button></div>';
  el.querySelector('h2').textContent = paused ? t.paused : t.other;
  el.querySelector('p').textContent = t.note;
  const button = el.querySelector('button');
  button.textContent = t.use;
  document.body.appendChild(el);
  return new Promise((resolve) => {
    button.addEventListener('click', () => {
      button.disabled = true;
      button.textContent = t.wait;
      resolve(el);
    }, { once: true });
  });
}

// Resolves once this page may run: at once when no other tab runs Capishot, else after "Use here"
// and the other tab stepping aside. `free`: no gate (a test opening several tabs).
export async function tab_claim(free) {
  if (free || !navigator.locks || !navigator.locks.request || !self.BroadcastChannel) return;
  let paused = false;
  try { paused = sessionStorage.getItem(PAUSED) === '1'; sessionStorage.removeItem(PAUSED); } catch (e) { /* none */ }
  if (!paused && (await hold({ ifAvailable: true }))) {
    listen();
    return;
  }
  const overlay = await ask(paused);
  // The other tab may be gone by now (closed, or this one was the paused one left behind).
  if (await hold({ ifAvailable: true })) {
    listen();
    overlay.remove();
    return;
  }
  const ask_ = new BroadcastChannel(CHANNEL);
  let last = Date.now();
  let released = false;
  ask_.onmessage = (e) => {
    if (e.data === 'saving') last = Date.now();
    if (e.data === 'released') released = true;
  };
  ask_.postMessage('takeover');
  while (!released && Date.now() - last < QUIET_MS) await sleep(150);
  ask_.close();
  // Released: the other tab is reloading and lets go of the lock as it does. Silent: taken.
  await hold(released ? {} : { steal: true });
  listen();
  overlay.remove();
}

// Whether another tab asked to take over.
export function tab_takeover_asked() {
  return takeover;
}

// The documents are checkpointed (or the wait is over): still saving, or done and stepping aside.
export function tab_saving() {
  if (channel) channel.postMessage('saving');
}

export function tab_release() {
  if (channel) channel.postMessage('released');
  pause();
}
