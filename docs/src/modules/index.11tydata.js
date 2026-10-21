// The development board's figures, computed from src/_data/modules.json at
// build time so the page renders complete without JavaScript.
//
// A card's progress and the completion figure count features: a done feature
// counts as one, an in-progress one as half, as the board always has.

const COLUMNS = [
    { status: "todo", label: "Todo", subtitle: "Not started yet" },
    { status: "progress", label: "In Progress", subtitle: "Partially implemented" },
    { status: "done", label: "Completed", subtitle: "Fully implemented and tested" },
];

const credit = (feature) => (feature.status === "done" ? 1 : feature.status === "progress" ? 0.5 : 0);

export default {
    eleventyComputed: {
        board: (data) => {
            const cards = Object.entries(data.modules).map(([key, card]) => {
                const total = card.features.length;
                const done = card.features.filter((feature) => feature.status === "done").length;
                const earned = card.features.reduce((sum, feature) => sum + credit(feature), 0);
                return { key, ...card, total, done, progress: Math.round((earned / total) * 100) };
            });
            const features = cards.flatMap((card) => card.features);
            const earned = features.reduce((sum, feature) => sum + credit(feature), 0);
            const count = (status) => cards.filter((card) => card.status === status).length;
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
                },
            };
        },
    },
};
