/* ============================================================
   原型辅助脚本：图标 sprite + 标签切换 + 缩放
   产品实现请改用 lucide-vue-next，图标名与此处一致。
   ============================================================ */

const ICONS = {
  'chevron-right': '<path d="M9 18l6-6-6-6"/>',
  'chevron-down': '<path d="M6 9l6 6 6-6"/>',
  'chevron-up': '<path d="M18 15l-6-6-6 6"/>',
  'chevron-left': '<path d="M15 18l-6-6 6-6"/>',
  'plus': '<path d="M12 5v14M5 12h14"/>',
  'minus': '<path d="M5 12h14"/>',
  'x': '<path d="M18 6L6 18M6 6l12 12"/>',
  'check': '<path d="M20 6L9 17l-5-5"/>',
  'search': '<circle cx="11" cy="11" r="8"/><path d="M21 21l-4.3-4.3"/>',
  'settings': '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 11-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 11-4 0v-.09A1.65 1.65 0 008.6 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 11-2.83-2.83l.06-.06A1.65 1.65 0 004.6 15a1.65 1.65 0 00-1.51-1H3a2 2 0 110-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 112.83-2.83l.06.06A1.65 1.65 0 009 4.6a1.65 1.65 0 001-1.51V3a2 2 0 114 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 112.83 2.83l-.06.06A1.65 1.65 0 0019.4 9v.09a1.65 1.65 0 001.51 1H21a2 2 0 110 4h-.09a1.65 1.65 0 00-1.51 1z"/>',
  'sliders': '<path d="M21 4h-7M10 4H3M21 12h-9M8 12H3M21 20h-5M12 20H3M14 2v4M8 10v4M16 18v4"/>',
  'refresh': '<path d="M21 12a9 9 0 11-2.64-6.36M21 3v6h-6"/>',
  'trash': '<path d="M3 6h18M8 6V4.5A1.5 1.5 0 019.5 3h5A1.5 1.5 0 0116 6M19 6l-.9 13.1A2 2 0 0116.1 21H7.9a2 2 0 01-2-1.9L5 6M10 11v6M14 11v6"/>',
  'copy': '<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/>',
  'pencil': '<path d="M17 3a2.83 2.83 0 114 4L7.5 20.5 2 22l1.5-5.5L17 3z"/>',
  'clock': '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3.5 2"/>',
  'lock': '<rect x="4" y="10" width="16" height="11" rx="2"/><path d="M8 10V7a4 4 0 018 0v3"/>',
  'database': '<ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v14c0 1.66 3.58 3 8 3s8-1.34 8-3V5"/><path d="M4 12c0 1.66 3.58 3 8 3s8-1.34 8-3"/>',
  'server': '<rect x="2" y="3" width="20" height="7" rx="2"/><rect x="2" y="14" width="20" height="7" rx="2"/><path d="M6 6.5h.01M6 17.5h.01"/>',
  'terminal': '<path d="M4 17l6-6-6-6M12 19h8"/>',
  'filter': '<path d="M21 4H3l7 8.5V19l4 2v-8.5L21 4z"/>',
  'more-h': '<path d="M5 12h.01M12 12h.01M19 12h.01" stroke-width="2.6"/>',
  'more-v': '<path d="M12 5v.01M12 12v.01M12 19v.01" stroke-width="2.6"/>',
  'alert': '<path d="M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L14.7 3.9a2 2 0 00-3.4 0z"/><path d="M12 9v4.5M12 17.2h.01"/>',
  'info': '<circle cx="12" cy="12" r="9"/><path d="M12 16v-4.5M12 8h.01"/>',
  'key': '<circle cx="7.5" cy="15.5" r="4.5"/><path d="M21 3l-10.3 10.3M15.5 8.5l3 3"/>',
  'folder': '<path d="M4 20a2 2 0 01-2-2V6a2 2 0 012-2h4.6l2 3H20a2 2 0 012 2v9a2 2 0 01-2 2H4z"/>',
  'play': '<path d="M7 4l13 8-13 8V4z"/>',
  'stop': '<rect x="5" y="5" width="14" height="14" rx="2"/>',
  'download': '<path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4M7 10l5 5 5-5M12 15V3"/>',
  'upload': '<path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4M17 8l-5-5-5 5M12 3v12"/>',
  'eye': '<path d="M2 12s3.8-7.5 10-7.5S22 12 22 12s-3.8 7.5-10 7.5S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
  'code': '<path d="M16 18l6-6-6-6M8 6l-6 6 6 6"/>',
  'braces': '<path d="M8 3H7a2 2 0 00-2 2v4a2 2 0 01-2 2 2 2 0 012 2v4a2 2 0 002 2h1M16 3h1a2 2 0 012 2v4a2 2 0 002 2 2 2 0 00-2 2v4a2 2 0 01-2 2h-1"/>',
  'shield': '<path d="M12 22s8-4 8-10V5.5L12 2 4 5.5V12c0 6 8 10 8 10z"/>',
  'activity': '<path d="M22 12h-4l-3 8-6-16-3 8H2"/>',
  'chart': '<path d="M12 20V9M18 20V4M6 20v-6"/>',
  'list': '<path d="M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01"/>',
  'keyboard': '<rect x="2" y="5" width="20" height="14" rx="2"/><path d="M6 9h.01M10 9h.01M14 9h.01M18 9h.01M6 13h.01M18 13h.01M9 16h6"/>',
  'external': '<path d="M18 13v6a2 2 0 01-2 2H5a2 2 0 01-2-2V8a2 2 0 012-2h6M15 3h6v6M10 14L21 3"/>',
  'link': '<path d="M10.6 13.4a5 5 0 007.5.54l3-3a5 5 0 00-7.07-7.07l-1.72 1.71"/><path d="M13.4 10.6a5 5 0 00-7.5-.54l-3 3a5 5 0 007.07 7.07l1.71-1.71"/>',
  'droplet': '<path d="M12 2.7l5.66 5.63a8 8 0 11-11.31 0L12 2.7z"/>',
  'hdd': '<path d="M22 12H2M5.45 5.11L2 12v6a2 2 0 002 2h16a2 2 0 002-2v-6l-3.45-6.89A2 2 0 0016.76 4H7.24a2 2 0 00-1.79 1.11z"/><path d="M6 16h.01M10 16h.01"/>',
  'enter': '<path d="M9 10l-5 5 5 5M20 4v7a4 4 0 01-4 4H4"/>',
  'branch': '<path d="M6 3v12"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 01-9 9"/>',
  'columns': '<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M15 3v18"/>',
  'sort': '<path d="M7 4v16M3.5 16.5L7 20l3.5-3.5M17 20V4M13.5 7.5L17 4l3.5 3.5"/>',
  'zap': '<path d="M13 2L4 14h7l-1 8 9-12h-7l1-8z"/>',
  'globe': '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 010 18 14 14 0 010-18z"/>',
  'type': '<path d="M4 7V4h16v3M9 20h6M12 4v16"/>',
  'power': '<path d="M12 3v9M18.4 6.6a9 9 0 11-12.8 0"/>',
  'maximize': '<rect x="3.5" y="3.5" width="17" height="17" rx="2"/>',
  'expand': '<path d="M15 3h6v6M9 21H3v-6M21 3l-7.5 7.5M3 21l7.5-7.5"/>',
  'file': '<path d="M14 2H7a2 2 0 00-2 2v16a2 2 0 002 2h10a2 2 0 002-2V7l-5-5z"/><path d="M14 2v5h5"/>',
  'folder-open': '<path d="M4 20h14.2a2 2 0 001.9-1.4L22 12H8.5a2 2 0 00-1.9 1.4L4 20zM4 20V6a2 2 0 012-2h4.6l2 3H18a2 2 0 012 2v3"/>',
  'wand': '<path d="M15 4V2M15 10V8M12.5 6h-2M19.5 6h-2M13.2 8.8l-1.4-1.4M17.6 4.4l-1.4-1.4M3 21L14 10l1.4 1.4L4.4 22.4 3 21z"/>',
  'history': '<path d="M3 12a9 9 0 109-9 9 9 0 00-6.4 2.7L3 8"/><path d="M3 3v5h5M12 7v5l3.5 2"/>',
  'bell': '<path d="M18 8a6 6 0 10-12 0c0 7-3 8-3 8h18s-3-1-3-8"/><path d="M13.7 21a2 2 0 01-3.4 0"/>',
  'save': '<path d="M19 21H5a2 2 0 01-2-2V5a2 2 0 012-2h11l5 5v11a2 2 0 01-2 2z"/><path d="M17 21v-8H7v8M7 3v5h8"/>',
};

function injectSprite() {
  const parts = Object.entries(ICONS).map(
    ([name, body]) => `<symbol id="i-${name}" viewBox="0 0 24 24">${body}</symbol>`
  );
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('style', 'position:absolute;width:0;height:0;overflow:hidden');
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '1.8');
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('stroke-linejoin', 'round');
  svg.innerHTML = parts.join('');
  document.body.insertBefore(svg, document.body.firstChild);
}

/* [data-tab-group] 容器内：[data-tab="x"] 触发，[data-panel="x"] 面板 */
function wireTabs() {
  document.querySelectorAll('[data-tab-group]').forEach((group) => {
    const name = group.getAttribute('data-tab-group');
    const scope = document.querySelector(`[data-tab-scope="${name}"]`) || group.closest('.win') || document;
    group.querySelectorAll('[data-tab]').forEach((trigger) => {
      trigger.addEventListener('click', () => {
        const id = trigger.getAttribute('data-tab');
        group.querySelectorAll('[data-tab]').forEach((t) =>
          t.classList.toggle('is-active', t === trigger)
        );
        scope.querySelectorAll(`[data-panel-group="${name}"]`).forEach((p) =>
          p.classList.toggle('is-hidden', p.getAttribute('data-panel') !== id)
        );
      });
    });
  });
}

function wireZoom() {
  const ctl = document.querySelector('[data-zoom-ctl]');
  if (!ctl) return;
  /* 用 zoom 而不是 transform：zoom 参与布局，缩小后页面不会留下空白或横向滚动条 */
  const apply = (z) => {
    document.querySelectorAll('.stage').forEach((stage) => {
      stage.style.zoom = z;
    });
    ctl.querySelectorAll('button').forEach((b) =>
      b.classList.toggle('is-active', b.dataset.zoom === String(z))
    );
  };
  ctl.querySelectorAll('button').forEach((b) =>
    b.addEventListener('click', () => apply(Number(b.dataset.zoom)))
  );
  const fit = Math.min(1, (window.innerWidth - 64) / 1280);
  apply(fit >= 0.95 ? 1 : fit >= 0.8 ? 0.8 : 0.65);
}

document.addEventListener('DOMContentLoaded', () => {
  injectSprite();
  wireTabs();
  wireZoom();
});
