import { fetchCurrentUser, renderHeader } from '../auth.js';
import { $, $$, storage } from '../ui.js';

const STORAGE_KEY = 'uptime-medics-course-progress';
const buttons = $$('[data-course-complete]');
const saved = storage.get(STORAGE_KEY, []);
const completed = new Set(Array.isArray(saved) ? saved.filter((id) => buttons.some((button) => button.dataset.courseComplete === id)) : []);

function renderProgress() {
    $('#course-progress').hidden = false;
    $('#course-progress-count').textContent = `${completed.size} of ${buttons.length} courses completed`;
    buttons.forEach((button) => {
        const isComplete = completed.has(button.dataset.courseComplete);
        button.hidden = false;
        button.setAttribute('aria-pressed', String(isComplete));
        button.textContent = isComplete ? 'Completed ✓ · Mark incomplete' : 'Mark complete';
    });
}

function openLinkedCourse() {
    const course = document.getElementById(location.hash.slice(1));
    if (course?.matches('details.course')) {
        course.open = true;
        course.scrollIntoView({ block: 'start' });
    }
}

buttons.forEach((button) => {
    button.addEventListener('click', () => {
        const id = button.dataset.courseComplete;
        if (completed.has(id)) completed.delete(id);
        else completed.add(id);
        storage.set(STORAGE_KEY, [...completed]);
        renderProgress();
    });
});

$$('.course-catalog a').forEach((link) => {
    link.addEventListener('click', () => {
        document.getElementById(link.hash.slice(1)).open = true;
    });
});
window.addEventListener('hashchange', openLinkedCourse);
renderProgress();
openLinkedCourse();
renderHeader(null);
fetchCurrentUser().then(renderHeader);
