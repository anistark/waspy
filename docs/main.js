function toggleTheme() {
    const html = document.documentElement;
    const next = html.getAttribute('data-theme') === 'light' ? 'dark' : 'light';
    html.setAttribute('data-theme', next);
    localStorage.setItem('theme', next);
}

document.addEventListener('DOMContentLoaded', () => {
    const toggle = document.getElementById('themeToggle');
    if (toggle) toggle.addEventListener('click', toggleTheme);

    const observer = new IntersectionObserver((entries) => {
        entries.forEach(entry => {
            if (entry.isIntersecting) {
                entry.target.classList.add('in-view');
                observer.unobserve(entry.target);
            }
        });
    }, { threshold: 0.1, rootMargin: '0px 0px -40px 0px' });

    document.querySelectorAll('.reveal').forEach(el => observer.observe(el));

    loadContributors();

    document.querySelectorAll('.copy-btn[data-copy]').forEach(btn => {
        btn.addEventListener('click', () => {
            navigator.clipboard.writeText(btn.dataset.copy).then(() => {
                const original = btn.textContent;
                btn.textContent = 'Copied ✓';
                btn.classList.add('copied');
                setTimeout(() => {
                    btn.textContent = original;
                    btn.classList.remove('copied');
                }, 2000);
            });
        });
    });
});

function loadContributors() {
    const grid = document.getElementById('contributorsGrid');
    if (!grid) return;

    const listUrl = 'https://github.com/anistark/waspy/graphs/contributors';

    const fail = () => {
        grid.innerHTML = '<p class="contributors-status">Could not load the list right now. ' +
            '<a href="' + listUrl + '">See contributors on GitHub</a>.</p>';
    };

    fetch('https://api.github.com/repos/anistark/waspy/contributors?per_page=100')
        .then(res => res.ok ? res.json() : Promise.reject(new Error(res.status)))
        .then(people => {
            const humans = people.filter(p =>
                p.type !== 'Bot' && !/\[bot\]$/.test(p.login || '')
            );

            if (!humans.length) return fail();

            grid.innerHTML = '';
            humans.forEach(p => {
                const a = document.createElement('a');
                a.className = 'contributor';
                a.href = p.html_url;
                a.target = '_blank';
                a.rel = 'noopener';
                a.title = p.login + ': ' + p.contributions + ' commits';

                const img = document.createElement('img');
                img.src = p.avatar_url + '&s=140';
                img.alt = p.login;
                img.loading = 'lazy';
                img.width = 64;
                img.height = 64;

                const name = document.createElement('span');
                name.className = 'contributor-name';
                name.textContent = p.login;

                const count = document.createElement('span');
                count.className = 'contributor-count';
                count.textContent = p.contributions + (p.contributions === 1 ? ' commit' : ' commits');

                a.append(img, name, count);
                grid.appendChild(a);
            });
        })
        .catch(fail);
}
