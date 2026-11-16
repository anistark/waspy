# heapq, CPython 3.13's pure-Python algorithms (Lib/heapq.py), compiled with
# the program. A function's parameters take the types its calls pass.


def heappush(heap, item):
    heap.append(item)
    _siftdown(heap, 0, len(heap) - 1)


def heappop(heap):
    lastelt = heap.pop()
    if heap:
        returnitem = heap[0]
        heap[0] = lastelt
        _siftup(heap, 0)
        return returnitem
    return lastelt


def heapreplace(heap, item):
    returnitem = heap[0]
    heap[0] = item
    _siftup(heap, 0)
    return returnitem


def heappushpop(heap, item):
    if heap and heap[0] < item:
        top = heap[0]
        heap[0] = item
        _siftup(heap, 0)
        return top
    return item


def heapify(x):
    n = len(x)
    i = n // 2 - 1
    while i >= 0:
        _siftup(x, i)
        i -= 1


def _siftdown(heap, startpos, pos):
    newitem = heap[pos]
    while pos > startpos:
        parentpos = (pos - 1) >> 1
        parent = heap[parentpos]
        if newitem < parent:
            heap[pos] = parent
            pos = parentpos
            continue
        break
    heap[pos] = newitem


def _siftup(heap, pos):
    endpos = len(heap)
    startpos = pos
    newitem = heap[pos]
    childpos = 2 * pos + 1
    while childpos < endpos:
        rightpos = childpos + 1
        if rightpos < endpos and not heap[childpos] < heap[rightpos]:
            childpos = rightpos
        heap[pos] = heap[childpos]
        pos = childpos
        childpos = 2 * pos + 1
    heap[pos] = newitem
    _siftdown(heap, startpos, pos)


def nlargest(n, iterable):
    # Equivalent to sorted(iterable, reverse=True)[:n], as documented.
    return sorted(iterable, reverse=True)[:n]


def nsmallest(n, iterable):
    # Equivalent to sorted(iterable)[:n], as documented.
    return sorted(iterable)[:n]
