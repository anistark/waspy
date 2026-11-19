// The development board's figures, computed from src/_data/modules.json at
// build time so the page renders complete without JavaScript.
//
// The page is built once per Python version in src/_data/python.json (the
// pagination in index.njk). A version's board holds every feature up to it:
// a feature with no `python` field is the first version's, and one marked
// `"python": "3.15"` joins the 3.15 board and every later one. A card's
// status follows from the features on the board being built.
//
// A card's progress and the completion figure count features: a done feature
// counts as one, an in-progress one as half, as the board always has.

const COLUMNS = [
    { status: "todo", label: "Todo", subtitle: "Not started yet" },
    { status: "progress", label: "In Progress", subtitle: "Partially implemented" },
    { status: "done", label: "Completed", subtitle: "Fully implemented and tested" },
];

const credit = (feature) => (feature.status === "done" ? 1 : feature.status === "progress" ? 0.5 : 0);

const statusOf = (features) =>
    features.every((feature) => feature.status === "done")
        ? "done"
        : features.every((feature) => feature.status === "todo")
          ? "todo"
          : "progress";

export default {
    eleventyComputed: {
        title: (data) => (data.py.version === data.python[0].version ? "Development board" : `Development board for Python ${data.py.version}`),
        description: (data) =>
            `What Waspy compiles of Python ${data.py.version}, feature by feature, each completed one backed by a test against CPython's answer.`,
        board: (data) => {
            const versions = data.python.map((entry) => entry.version);
            const base = versions[0];
            const index = versions.indexOf(data.py.version);
            const reached = (feature) => versions.indexOf(feature.python ?? base) <= index;
            const isAdded = (feature) => index > 0 && feature.python === data.py.version;

            const cards = Object.entries(data.modules).map(([key, card]) => {
                const features = card.features.filter(reached).map((feature) => ({
                    ...feature,
                    added: isAdded(feature),
                    since: feature.python && feature.python !== base ? feature.python : null,
                }));
                const total = features.length;
                const done = features.filter((feature) => feature.status === "done").length;
                const earned = features.reduce((sum, feature) => sum + credit(feature), 0);
                const added = features.filter((feature) => feature.added).length;
                return {
                    key,
                    title: card.title,
                    section: card.section,
                    features,
                    total,
                    done,
                    added,
                    status: statusOf(features),
                    progress: Math.round((earned / total) * 100),
                };
            });
            const features = cards.flatMap((card) => card.features);
            const earned = features.reduce((sum, feature) => sum + credit(feature), 0);
            const count = (status) => cards.filter((card) => card.status === status).length;
            const added = features.filter((feature) => feature.added);
            return {
                columns: COLUMNS.map((column) => ({
                    ...column,
                    cards: cards.filter((card) => card.status === column.status),
                })),
                stats: {
                    done: count("done"),
                    progress: count("progress"),
                    todo: count("todo"),
                    cards: cards.length,
                    features: features.length,
                    featuresDone: features.filter((feature) => feature.status === "done").length,
                    completion: Math.round((earned / features.length) * 100),
                    added: added.length,
                    addedDone: added.filter((feature) => feature.status === "done").length,
                    addedCards: cards.filter((card) => card.added > 0).length,
                },
            };
        },
    },
};
