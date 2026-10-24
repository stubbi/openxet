import { describeUpload } from './file-example.mjs';

const example = document.querySelector('[data-file-example]');
if (example) {
  const stored = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'];
  const scenarios = {
    small: ['A', 'B', 'I', 'D', 'E', 'F', 'G', 'H'],
    same: stored,
    new: ['I', 'J', 'K', 'L', 'M', 'N', 'O', 'P'],
  };
  const explanations = {
    small: 'Your file stays whole. Only the new part needs uploading.',
    same: 'Already stored? All eight parts can be reused. No new file data to upload.',
    new: 'Nothing matches yet. All eight parts need uploading this time.',
  };
  const controls = example.querySelector('[data-example-controls]');
  const result = example.querySelector('[data-example-result]');
  const explanation = example.querySelector('[data-example-explanation]');
  const cells = example.querySelectorAll('[data-updated-part]');
  const uploadTray = example.querySelector('[data-upload-tray]');
  controls.addEventListener('click', event => {
    const button = event.target.closest('button[data-scenario]');
    if (!button) return;
    const scenario = button.dataset.scenario;
    const upload = describeUpload(stored, scenarios[scenario]);
    upload.parts.forEach((part, index) => {
      cells[index].textContent = part.label + (part.reused ? '' : '+');
      cells[index].classList.toggle('is-new', !part.reused);
    });
    const newParts = upload.uploadParts.map(label => {
      const part = document.createElement('span');
      part.className = 'is-new';
      part.textContent = `${label}+`;
      return part;
    });
    if (!newParts.length) {
      const empty = document.createElement('p');
      empty.className = 'transfer-empty';
      empty.textContent = 'None — all parts are already stored.';
      newParts.push(empty);
    }
    uploadTray.replaceChildren(...newParts);
    for (const control of controls.querySelectorAll('button')) {
      control.setAttribute('aria-pressed', String(control === button));
    }
    result.textContent = `${upload.reusedCount} parts reused · ${upload.newCount} new ${upload.newCount === 1 ? 'part' : 'parts'} to upload`;
    explanation.textContent = explanations[scenario];
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
