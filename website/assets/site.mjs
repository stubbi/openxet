import { describeUpload } from './file-example.mjs';

const example = document.querySelector('[data-file-example]');
if (example) {
  const stored = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'];
  const scenarios = {
    small: ['A', 'B', 'I', 'D', 'E', 'F', 'G', 'H'],
    same: stored,
    new: ['I', 'J', 'K', 'L', 'M', 'N', 'O', 'P'],
  };
  const controls = example.querySelector('[data-example-controls]');
  const result = example.querySelector('[data-example-result]');
  const cells = example.querySelectorAll('[data-updated-part]');
  controls.addEventListener('click', event => {
    const button = event.target.closest('button[data-scenario]');
    if (!button) return;
    const upload = describeUpload(stored, scenarios[button.dataset.scenario]);
    upload.parts.forEach((part, index) => cells[index].classList.toggle('is-new', !part.reused));
    for (const control of controls.querySelectorAll('button')) {
      control.setAttribute('aria-pressed', String(control === button));
    }
    result.textContent = `${upload.newCount} / ${stored.length} parts sent`;
  });
  controls.hidden = false;
}

for (const menu of document.querySelectorAll('.mobile-menu')) {
  menu.addEventListener('click', event => {
    if (event.target.closest('a')) menu.open = false;
  });
  menu.addEventListener('keydown', event => {
    if (event.key === 'Escape' && menu.open) {
      menu.open = false;
      menu.querySelector('summary').focus();
    }
  });
}
